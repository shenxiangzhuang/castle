//! The unique session command owner; execution is always joined before acknowledgement.
use super::*;

pub(super) struct Owner {
    pub(super) agent: Option<Agent>,
    pub(super) active: Option<ActiveAgent>,
    pub(super) snapshot: RuntimeSnapshot,
    pub(super) project: String,
    pub(super) directory: Option<PathBuf>,
    pub(super) published: Arc<Mutex<Published>>,
    pub(super) stop_replies: Vec<(crate::TxId, Reply)>,
    pub(super) admission: Option<(InputId, crate::TxId, Reply)>,
    pub(super) terminal: Option<RuntimeStatus>,
    pub(super) shutting_down: bool,
    pub(super) ledger: Option<crate::session::store::SessionStore>,
    pub(super) stopping: tokio_util::sync::CancellationToken,
}

impl Drop for Owner {
    fn drop(&mut self) {
        self.published
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .updates = None;
    }
}

impl Owner {
    fn publish(&self, event: SessionUpdate) {
        if let Ok(mut published) = self.published.lock() {
            published.snapshot = self.snapshot.clone();
            if let Some(updates) = &published.updates {
                let _ = updates.send(event);
            }
        }
    }
    fn changed(&self) {
        self.publish(SessionUpdate::Changed(Box::new(self.snapshot.clone())));
    }
    fn notify_result(&self, id: crate::TxId, reply: Reply, result: Result<CommandResult, String>) {
        self.publish(SessionUpdate::CommandResult {
            id,
            result: result.clone(),
        });
        let _ = reply.send(result);
    }
    async fn reserve(
        &mut self,
        id: &crate::TxId,
        command: &SessionCommand,
    ) -> Result<Option<Result<CommandResult, String>>, String> {
        if self.ledger.is_none() {
            let directory = self.directory.clone();
            let store = self
                .agent
                .as_ref()
                .ok_or("agent unavailable")?
                .store
                .clone();
            self.ledger = Some(
                tokio::task::spawn_blocking(move || {
                    directory.map_or(Ok(store), crate::session::store::SessionStore::open_project)
                })
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?,
            );
        }
        let store = self.ledger.clone().ok_or("command store unavailable")?;
        let session = self.snapshot.session.id.clone();
        let id = id.clone();
        // Credentials are connection setup, not part of the durable semantic command.
        let request = serde_json::to_vec(command).map_err(|e| e.to_string())?;
        let previous =
            tokio::task::spawn_blocking(move || store.reserve_command(&session, &id, &request))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
        previous
            .map(|bytes| serde_json::from_slice(&bytes).map_err(|e| e.to_string()))
            .transpose()
    }

    async fn reply(&self, id: crate::TxId, reply: Reply, result: Result<CommandResult, String>) {
        let recorded = async {
            let store = self
                .ledger
                .clone()
                .ok_or("command store unavailable".to_owned())?;
            let session = self.snapshot.session.id.clone();
            let command_id = id.clone();
            let bytes = serde_json::to_vec(&result).map_err(|e| e.to_string())?;
            tokio::task::spawn_blocking(move || store.finish_command(&session, &command_id, &bytes))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())
        }
        .await;
        let result = match recorded {
            Ok(()) => result,
            Err(error) => Err(format!("command outcome unknown: {error}")),
        };
        self.notify_result(id, reply, result);
    }

    fn sync_agent(&mut self) {
        if let Some(agent) = &self.agent {
            self.snapshot.session = agent.info.clone();
            self.snapshot.config = agent.session_config.clone();
            self.snapshot.revision = agent.revision;
            self.snapshot.can_branch = agent.can_change_conversation();
        }
    }
    pub(super) async fn run(mut self, mut commands: mpsc::Receiver<Envelope>) {
        let mut connected = true;
        loop {
            if let Some(active) = &mut self.active {
                tokio::select! {
                    _ = self.stopping.cancelled(), if !self.shutting_down => {
                        self.shutting_down = true;
                        active.control().abort();
                    }
                    event = active.next_event() => {
                        match event {
                            Some(event) => self.observe(event).await,
                            None => self.settle().await,
                        }
                    }
                    command = commands.recv(), if connected => {
                        match command {
                            Some(command) => self.command(command).await,
                            None => connected = false,
                        }
                    }
                }
            } else if self.shutting_down || !connected {
                break;
            } else {
                tokio::select! {
                    _ = self.stopping.cancelled() => break,
                    command = commands.recv() => match command {
                        Some(command) => self.command(command).await,
                        None => break,
                    }
                }
            }
        }
    }

    async fn command(&mut self, envelope: Envelope) {
        let Envelope { id, command, reply } = envelope;
        match self.reserve(&id, &command).await {
            Ok(Some(previous)) => {
                self.notify_result(id, reply, previous);
                return;
            }
            Err(error) => {
                self.notify_result(id, reply, Err(error));
                return;
            }
            Ok(None) => {}
        }
        if self.shutting_down {
            self.reply(id, reply, Err("session is shutting down".into()))
                .await;
            return;
        }
        if let SessionCommand::Stop { run } = &command
            && *run != self.snapshot.run
        {
            self.reply(id, reply, Err("stale run".into())).await;
            return;
        }
        if matches!(
            command,
            SessionCommand::Stop { .. } | SessionCommand::Shutdown
        ) {
            self.shutting_down = matches!(command, SessionCommand::Shutdown);
            if let Some(active) = &self.active {
                active.control().abort();
                self.stop_replies.push((id, reply));
            } else {
                self.reply(id, reply, Ok(CommandResult::Applied)).await;
            }
            return;
        }
        if self.snapshot.session.is_archived() {
            self.reply(id, reply, Err("session is archived".into()))
                .await;
            return;
        }
        if self.active.is_some() {
            let result = self.active_command(command).await;
            while let Some(event) = self.active.as_mut().and_then(ActiveAgent::try_next_event) {
                self.observe(event).await;
            }
            self.reply(id, reply, result).await;
            self.changed();
            return;
        }
        match command {
            SessionCommand::Submit {
                id: input_id,
                text,
                mode: SubmitMode::Start,
            } => {
                self.start(Some((input_id, text)), None, Some((id, reply)))
                    .await;
            }
            SessionCommand::Edit {
                id: input_id,
                text,
                target,
                revision,
                head,
            } => {
                self.start(
                    Some((input_id, text)),
                    Some((target, revision, head)),
                    Some((id, reply)),
                )
                .await;
            }
            SessionCommand::Compact { instructions } => {
                self.begin_run();
                match self.prepare_run(None).await {
                    Ok(()) => {
                        if let Some(agent) = self.agent.take() {
                            self.stop_replies.push((id, reply));
                            self.activate(agent.start_compaction(instructions));
                        } else {
                            self.reply(id, reply, Err("agent unavailable".into())).await;
                        }
                    }
                    Err(error) => self.reply(id, reply, Err(error)).await,
                }
            }
            SessionCommand::ResumePending => {
                self.start(None, None, Some((id, reply))).await;
            }
            other => {
                let result = self.idle_command(other).await;
                self.sync_agent();
                self.snapshot.generation = self.snapshot.generation.saturating_add(1);
                self.snapshot.status = RuntimeStatus::Idle;
                self.changed();
                self.reply(id, reply, result).await;
            }
        }
    }

    async fn active_command(&mut self, command: SessionCommand) -> Result<CommandResult, String> {
        if self.snapshot.status != RuntimeStatus::Running {
            return Err("session is settling".into());
        }
        let control = self.active.as_ref().ok_or("run is unavailable")?.control();
        let result = match command {
            SessionCommand::Submit {
                id,
                text,
                mode: mode @ (SubmitMode::Queue | SubmitMode::Steer),
            } => {
                let origin = match mode {
                    SubmitMode::Steer => InputOrigin::Steer,
                    _ => InputOrigin::Queue,
                };
                control.submit_input(id, text, origin).await
            }
            SessionCommand::CancelInput(input) => control.cancel_input(input).await,
            SessionCommand::Prioritize(input) => control.prioritize(input).await,
            SessionCommand::Decide {
                run,
                call_id,
                allow,
            } => {
                if run != self.snapshot.run {
                    return Err("stale approval run".into());
                }
                let result = if allow {
                    control.approve(call_id.clone()).await
                } else {
                    control.deny(call_id.clone()).await
                };
                if result.is_ok() {
                    self.snapshot
                        .approvals
                        .retain(|call| call.call_id != call_id);
                }
                result
            }
            SessionCommand::Configure {
                config,
                model: None,
            } if config.model == self.snapshot.config.model => {
                let result = control.set_permission(config.allow_all_tools).await;
                if result.is_ok() {
                    self.snapshot.config = config;
                    self.snapshot.generation = self.snapshot.generation.saturating_add(1);
                    self.approve_automatic().await?;
                }
                result
            }
            _ => return Err("operation requires an idle session".into()),
        };
        result
            .map(|_| CommandResult::Applied)
            .map_err(|error| error.to_string())
    }

    async fn idle_command(&mut self, command: SessionCommand) -> Result<CommandResult, String> {
        self.snapshot.status = RuntimeStatus::Configuring;
        self.changed();
        let agent = self.agent.as_mut().ok_or("agent is unavailable")?;
        match command {
            SessionCommand::Configure { config, model } => {
                let selected = model.as_ref().unwrap_or_else(|| agent.model());
                if let Some(effort) = config.model.reasoning_effort
                    && !selected.reasoning_efforts().contains(&effort)
                {
                    return Err(format!(
                        "unsupported reasoning effort for {}",
                        selected.model()
                    ));
                }
                agent
                    .persist_session_config(&config)
                    .await
                    .map_err(|e| e.to_string())?;
                if let Some(model) = model {
                    agent.set_model(model);
                }
            }
            SessionCommand::RefreshModel {
                expected_model,
                model,
            } => {
                if agent.session_config.model.model_id != expected_model {
                    return Err("model selection changed before connection refresh".into());
                }
                agent.set_model(model);
            }
            SessionCommand::Rename(title) => agent
                .rename_session(&title)
                .await
                .map_err(|e| e.to_string())?,
            SessionCommand::CancelInput(input) => {
                let agent = self.agent.take().ok_or("agent is unavailable")?;
                let (agent, receipts, result) = agent.cancel_pending_input(input).await;
                self.agent = Some(agent);
                for receipt in receipts {
                    self.committed(receipt).await;
                }
                result.map_err(|e| e.to_string())?;
            }
            SessionCommand::Fork {
                child,
                anchor,
                target,
                revision,
                head,
            } => {
                let agent = self.agent.take().ok_or("agent is unavailable")?;
                let (agent, receipts, result) = agent
                    .fork_session(child, anchor, target, revision, head)
                    .await;
                self.agent = Some(agent);
                for receipt in receipts {
                    self.committed(receipt).await;
                }
                return result
                    .map(|session| CommandResult::Forked(session.info().clone()))
                    .map_err(|e| e.to_string());
            }
            _ => return Err("operation requires a running session".into()),
        }
        Ok(CommandResult::Applied)
    }

    async fn start(
        &mut self,
        input: Option<(InputId, String)>,
        edit: Option<(InputId, u64, Option<u64>)>,
        reply: Option<(crate::TxId, Reply)>,
    ) {
        self.begin_run();
        let result = self
            .prepare_run(input.as_ref().map(|(_, text)| text.as_str()))
            .await;
        if let Err(error) = result {
            self.snapshot.status = RuntimeStatus::Failed(error.clone().into());
            if let Some((id, reply)) = reply {
                self.reply(id, reply, Err(error)).await;
            }
            self.changed();
            return;
        }
        let Some(agent) = self.agent.take() else {
            return;
        };
        let active = match input {
            Some((input_id, text)) => {
                if let Some((id, reply)) = reply {
                    self.admission = Some((input_id.clone(), id, reply));
                }
                if let Some((target, revision, head)) = edit {
                    agent.edit_input(input_id, text, target, revision, head)
                } else {
                    agent.start_input(input_id, text)
                }
            }
            None => {
                let active = agent.resume_pending();
                // Resume acknowledgement is completed at settlement, never before admission.
                if let Some(reply) = reply {
                    self.stop_replies.push(reply);
                }
                active
            }
        };
        self.activate(active);
    }

    fn begin_run(&mut self) {
        self.snapshot.run = crate::RunId::random();
        self.snapshot.run_number = self.snapshot.run_number.saturating_add(1);
    }

    fn activate(&mut self, active: ActiveAgent) {
        self.active = Some(active);
        self.terminal = None;
        self.snapshot.summary = None;
        self.snapshot.status = RuntimeStatus::Running;
        self.snapshot.can_branch = false;
        self.snapshot.started_at = Some(Instant::now());
        self.changed();
    }

    async fn prepare_run(&mut self, input: Option<&str>) -> Result<(), String> {
        if input.is_some_and(|text| text.trim().is_empty()) {
            return Err("agent input must not be empty".into());
        }
        if self.snapshot.session.path.as_os_str().is_empty() {
            if let Some(directory) = &self.directory {
                self.snapshot.status = RuntimeStatus::Creating;
                self.changed();
                let title = input
                    .and_then(crate::config::initial_session_title)
                    .unwrap_or_else(|| "Untitled session".into());
                let session = Session::create_named_in_project_with_id(
                    directory,
                    self.project.clone(),
                    self.snapshot.config.clone(),
                    self.snapshot.session.id.clone(),
                    title,
                )
                .await
                .map_err(|e| e.to_string())?;
                self.agent
                    .as_mut()
                    .ok_or("agent unavailable")?
                    .set_session(session);
            } else {
                self.agent
                    .as_mut()
                    .ok_or("agent unavailable")?
                    .persist_session_config(&self.snapshot.config)
                    .await
                    .map_err(|e| e.to_string())?;
            }
        }
        self.sync_agent();
        Ok(())
    }

    async fn committed(&mut self, receipt: CommitReceipt) {
        // A reload can return already observed transactions; never duplicate the projection.
        if receipt.revision <= self.snapshot.revision {
            return;
        }
        self.snapshot.events.extend(receipt.events.iter().cloned());
        self.snapshot.revision = receipt.revision;
        self.snapshot.session.updated_at = self
            .snapshot
            .session
            .updated_at
            .max(u64::try_from(receipt.committed_at_ms.max(0)).unwrap_or_default() / 1000);
        let accepted = self.admission.as_ref().is_some_and(|(input_id,_,_)| receipt.events.iter().any(|record| matches!(&record.event, crate::SessionEvent::InputSubmitted { input_id: id, .. } if id == input_id)));
        self.publish(SessionUpdate::Committed(receipt));
        if accepted && let Some((_, id, reply)) = self.admission.take() {
            self.reply(id, reply, Ok(CommandResult::Applied)).await;
        }
    }

    async fn observe(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::SessionCommitted(receipt) => self.committed(receipt).await,
            AgentEvent::ApprovalRequired(call) => {
                if !self
                    .snapshot
                    .approvals
                    .iter()
                    .any(|pending| pending.call_id == call.call_id)
                {
                    self.snapshot.approvals.push_back(call);
                }
                if let Err(error) = self.approve_automatic().await {
                    self.terminal = Some(RuntimeStatus::Failed(error.into()));
                    if let Some(active) = &self.active {
                        active.control().abort();
                    }
                }
            }
            AgentEvent::ConfigChanged(config) => self.snapshot.config = config,
            AgentEvent::RunFinished(summary) => {
                self.snapshot.summary = Some(summary);
                if self.terminal.is_none() {
                    self.snapshot.completed_runs += 1;
                }
                self.terminal.get_or_insert(RuntimeStatus::Idle);
                self.snapshot.status = RuntimeStatus::Settling;
                self.snapshot.approvals.clear();
            }
            AgentEvent::RunAborted => {
                self.terminal.get_or_insert(RuntimeStatus::Idle);
                self.snapshot.status = RuntimeStatus::Settling;
                self.snapshot.approvals.clear();
            }
            AgentEvent::RunFailed(error) => {
                self.terminal = Some(RuntimeStatus::Failed(error));
                self.snapshot.status = RuntimeStatus::Settling;
                self.snapshot.approvals.clear();
            }
        }
        self.changed();
    }

    async fn approve_automatic(&mut self) -> Result<(), String> {
        if !self.snapshot.config.allow_all_tools {
            return Ok(());
        }
        if let Some(active) = &self.active {
            let control = active.control();
            while let Some(call) = self.snapshot.approvals.front() {
                control
                    .approve(call.call_id.clone())
                    .await
                    .map_err(|e| e.to_string())?;
                self.snapshot.approvals.pop_front();
            }
        }
        Ok(())
    }

    async fn settle(&mut self) {
        let Some(active) = self.active.take() else {
            return;
        };
        self.snapshot.status = RuntimeStatus::Settling;
        self.changed();
        match active.finish().await {
            Ok(agent) => {
                self.agent = Some(agent);
                self.sync_agent();
                self.snapshot.status = self.terminal.take().unwrap_or_else(|| {
                    RuntimeStatus::Failed("run ended without terminal event".into())
                });
            }
            Err(error) => self.snapshot.status = RuntimeStatus::Failed(error.to_string().into()),
        }
        if let Some((_, id, reply)) = self.admission.take() {
            self.reply(
                id,
                reply,
                Err(
                    "Input acceptance was not confirmed; reopen the session before retrying".into(),
                ),
            )
            .await;
        }
        self.snapshot.started_at = None;
        self.snapshot.approvals.clear();
        self.changed();
        let result = match &self.snapshot.status {
            RuntimeStatus::Failed(error) => Err(error.message().to_owned()),
            _ => Ok(CommandResult::Applied),
        };
        for (id, reply) in std::mem::take(&mut self.stop_replies) {
            self.reply(id, reply, result.clone()).await;
        }
    }
}
