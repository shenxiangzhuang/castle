use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use gpui_kit::{Context, Task, Window};
pub(crate) use harness::RuntimeStatus as SessionConnectionStatus;
use harness::{
    CommandResult, CommitReceipt, InputId, PendingInput, ReasoningEffort, RunFailure,
    RuntimeSnapshot, Session, SessionCommand, SessionConfig, SessionEvent, SessionHandle,
    SessionInfo, SessionSetup, SessionUpdate, SubmitMode,
};
#[derive(Default)]
pub(crate) struct ApplicationHarness(pub(crate) harness::Harness);
impl gpui_kit::Global for ApplicationHarness {}
use crate::session::document::SessionDocument;
use crate::{
    app::ApprovalState,
    session::{RunId, SessionView},
};
use harness::config::ConfiguredModel;
use tokio::sync::{broadcast, oneshot};

#[derive(Clone, Debug)]
pub(crate) struct SessionConnectionSnapshot {
    pub(crate) session: SessionInfo,
    pub(crate) view: Arc<SessionView>,
    pub(crate) status: SessionConnectionStatus,
    pub(crate) approval: Option<ApprovalState>,
    pub(crate) started_at: Option<Instant>,
    pub(crate) allow_all_tools: bool,
    pub(crate) config: SessionConfig,
    pub(crate) active_run: Option<RunId>,
}

#[derive(Clone, Debug)]
pub(crate) struct SessionConnectionObservation {
    pub(crate) session: SessionInfo,
    pub(crate) status: SessionConnectionStatus,
    pub(crate) approval_needed: bool,
    pub(crate) completed_runs: u64,
    pub(crate) transcript_updates: u64,
    pub(crate) durable_revision: u64,
    pub(crate) metadata_generation: u64,
}

/// A UI projection and command adapter. Execution is owned by the harness.
pub(crate) struct SessionConnection {
    host: SessionHandle,
    observation_task: Option<Task<()>>,
    session: SessionInfo,
    sessions_dir: PathBuf,
    status: SessionConnectionStatus,
    active_run: Option<RunId>,
    run_token: Option<harness::RunId>,
    completed_runs: u64,
    transcript_updates: u64,
    durable_revision: u64,
    metadata_generation: u64,
    document: SessionDocument,
    display_document: Option<SessionDocument>,
    pub(crate) fork_children: Vec<(SessionInfo, harness::ForkOrigin)>,
    view: Arc<SessionView>,
    approval: Option<ApprovalState>,
    started_at: Option<Instant>,
    config: SessionConfig,
    tool_schemas: HashMap<String, String>,
    pending_commands: usize,
    pub(crate) input_action_pending: bool,
    pub(crate) input_error: Option<String>,
}

impl SessionConnection {
    pub(crate) fn new(
        harness: &harness::Harness,
        setup: SessionSetup,
        project_id: String,
        sessions_dir: PathBuf,
        document: SessionDocument,
        config: SessionConfig,
    ) -> Self {
        let tool_schemas = setup
            .tool_schemas()
            .into_iter()
            .filter_map(|schema| {
                let value = serde_json::to_value(schema).ok()?;
                let function = value.get("function").unwrap_or(&value);
                Some((
                    function.get("name")?.as_str()?.to_owned(),
                    serde_json::to_string_pretty(function).ok()?,
                ))
            })
            .collect();
        let session = setup.session_info().clone();
        let fork_children = Session::fork_children(&sessions_dir, &session.id).unwrap_or_default();
        let host = harness.attach(
            setup,
            project_id,
            Some(sessions_dir.clone()),
            config.clone(),
        );
        let durable_revision = host.snapshot().map_or(0, |snapshot| snapshot.revision);
        let view = Arc::new(SessionView::from_document(
            &document,
            &session.title,
            &tool_schemas,
            None,
        ));
        let mut runtime = Self {
            host,
            observation_task: None,
            session,
            sessions_dir,
            status: SessionConnectionStatus::Idle,
            active_run: None,
            run_token: None,
            completed_runs: 0,
            transcript_updates: 0,
            durable_revision,
            metadata_generation: 0,
            document,
            display_document: None,
            fork_children,
            view,
            approval: None,
            started_at: None,
            config,
            tool_schemas,
            pending_commands: 0,
            input_action_pending: false,
            input_error: None,
        };
        if runtime.document.tree.has_branches()
            || runtime.document.tree.origin.is_some()
            || runtime.document.tree.head()
                != runtime
                    .document
                    .tree
                    .nodes()
                    .next_back()
                    .map(|node| node.id)
        {
            runtime.rebuild_display();
        }
        runtime
    }

    pub(crate) fn snapshot(&self) -> SessionConnectionSnapshot {
        SessionConnectionSnapshot {
            session: self.session.clone(),
            view: self.view.clone(),
            status: self.status.clone(),
            approval: self.approval.clone(),
            started_at: self.started_at,
            allow_all_tools: self.config.allow_all_tools,
            config: self.config.clone(),
            active_run: self.active_run,
        }
    }
    pub(crate) fn observation(&self) -> SessionConnectionObservation {
        SessionConnectionObservation {
            session: self.session.clone(),
            status: self.status.clone(),
            approval_needed: self.approval.is_some(),
            completed_runs: self.completed_runs,
            transcript_updates: self.transcript_updates,
            durable_revision: self.durable_revision,
            metadata_generation: self.metadata_generation,
        }
    }
    pub(crate) fn is_active(&self) -> bool {
        self.pending_commands > 0 || self.status.is_active()
    }
    pub(crate) fn matches_loaded_session(&self, session: &Session) -> bool {
        !self.is_active()
            && self.durable_revision == session.revision()
            && self.session == *session.info()
            && self.config == *session.config()
    }
    #[cfg(test)]
    pub(crate) fn mark_failed_for_test(&mut self, message: impl Into<String>) {
        self.status = SessionConnectionStatus::Failed(message.into().into());
    }

    fn observe(&mut self, cx: &mut Context<Self>) {
        if self.observation_task.is_some() {
            return;
        }
        let host = self.host.clone();
        let connection = match host.connect() {
            Ok(connection) => connection,
            Err(error) => {
                self.fail_runtime(error);
                return;
            }
        };
        self.apply_snapshot(connection.snapshot);
        let mut events = connection.events;
        self.observation_task = Some(cx.spawn(async move |this, cx| {
            loop {
                match events.recv().await {
                    Ok(event) => {
                        if this
                            .update(cx, |runtime, cx| {
                                match event {
                                    SessionUpdate::Committed(receipt) => {
                                        runtime.apply_receipt(receipt)
                                    }
                                    SessionUpdate::Changed(snapshot) => {
                                        runtime.apply_snapshot(*snapshot)
                                    }
                                    SessionUpdate::CommandResult { .. } => {}
                                }
                                cx.notify();
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        let connection = match host.connect() {
                            Ok(connection) => connection,
                            Err(_) => break,
                        };
                        events = connection.events;
                        if this
                            .update(cx, |runtime, cx| {
                                match SessionDocument::from_events(
                                    connection.snapshot.events.iter().cloned().collect(),
                                ) {
                                    Ok(document) => {
                                        runtime.document = document;
                                        runtime.display_document = None;
                                        runtime.durable_revision = connection.snapshot.revision;
                                        runtime.rebuild_display();
                                        runtime.apply_snapshot(connection.snapshot);
                                    }
                                    Err(error) => runtime.fail_runtime(error.to_string()),
                                }
                                cx.notify();
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }));
    }

    fn apply_snapshot(&mut self, snapshot: RuntimeSnapshot) {
        let title_changed = self.session.title != snapshot.session.title;
        self.session = snapshot.session;
        self.status = snapshot.status;
        self.config = snapshot.config;
        self.metadata_generation = snapshot.generation;
        self.completed_runs = snapshot.completed_runs;
        self.active_run = self
            .status
            .is_active()
            .then_some(RunId(snapshot.run_number));
        self.run_token = self.status.is_active().then_some(snapshot.run);
        self.started_at = snapshot.started_at;
        self.approval = snapshot.approvals.front().map(|call| ApprovalState {
            call_id: call.call_id.clone(),
            name: call.name.clone(),
            arguments: call.arguments.clone(),
        });
        if title_changed {
            self.refresh_view();
        }
    }

    fn send(
        &mut self,
        command: SessionCommand,
        cx: &mut Context<Self>,
    ) -> Option<oneshot::Receiver<Result<CommandResult, String>>> {
        self.observe(cx);
        match self.host.try_send(command) {
            Ok(received) => {
                self.pending_commands += 1;
                let (sender, receiver) = oneshot::channel();
                cx.spawn(async move |this, cx| {
                    let result = received.await.unwrap_or_else(|_| {
                        Err("session owner ended before acknowledgement".into())
                    });
                    let _ = this.update(cx, |runtime, cx| {
                        runtime.pending_commands = runtime.pending_commands.saturating_sub(1);
                        if let Err(error) = &result {
                            runtime.input_error = Some(error.clone());
                        }
                        cx.notify();
                    });
                    let _ = sender.send(result);
                })
                .detach();
                cx.notify();
                Some(receiver)
            }
            Err(error) => {
                self.input_error = Some(error);
                cx.notify();
                None
            }
        }
    }

    fn configure(
        &mut self,
        config: SessionConfig,
        model: Option<harness::Model>,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.session.is_archived() {
            return false;
        }
        self.send(SessionCommand::Configure { config, model }, cx)
            .is_some()
    }
    pub(crate) fn set_allow_all_tools(&mut self, allow: bool, cx: &mut Context<Self>) -> bool {
        if allow == self.config.allow_all_tools {
            return true;
        }
        let mut config = self.config.clone();
        config.allow_all_tools = allow;
        self.configure(config, None, cx)
    }
    pub(crate) fn select_model(
        &mut self,
        configured: &ConfiguredModel,
        cx: &mut Context<Self>,
    ) -> bool {
        let mut config = self.config.clone();
        config.model = configured.session_model_config();
        self.configure(config, Some(configured.model.clone()), cx)
    }
    pub(crate) fn refresh_model(
        &mut self,
        configured: &ConfiguredModel,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.is_active() || self.config.model.model_id.as_deref() != Some(&configured.id) {
            return false;
        }
        self.send(
            SessionCommand::RefreshModel {
                expected_model: self.config.model.model_id.clone(),
                model: configured.model.clone(),
            },
            cx,
        )
        .is_some()
    }
    pub(crate) fn set_reasoning_effort(
        &mut self,
        effort: ReasoningEffort,
        cx: &mut Context<Self>,
    ) -> bool {
        let mut config = self.config.clone();
        config.model.reasoning_effort = Some(effort);
        self.configure(config, None, cx)
    }
    pub(crate) fn rename(&mut self, title: String, cx: &mut Context<Self>) -> bool {
        if self.is_active() || self.session.path.as_os_str().is_empty() {
            return false;
        }
        self.send(SessionCommand::Rename(title), cx).is_some()
    }
    pub(crate) fn refresh_fork_children(&mut self, cx: &mut Context<Self>) {
        match Session::fork_children(&self.sessions_dir, &self.session.id) {
            Ok(children) if children != self.fork_children => {
                self.fork_children = children;
                cx.notify();
            }
            Ok(_) => {}
            Err(error) => {
                self.input_error = Some(format!("Could not refresh fork relationships: {error}"))
            }
        }
    }
    pub(crate) fn tree(&self) -> &harness::ConversationTree {
        &self.document.tree
    }
    pub(crate) fn can_branch(&self) -> bool {
        !self.is_active()
            && !self.input_action_pending
            && !self.session.is_archived()
            && !self.session.path.as_os_str().is_empty()
            && self
                .host
                .snapshot()
                .is_ok_and(|snapshot| snapshot.can_branch)
    }
    pub(crate) fn pending_inputs(&self) -> Vec<PendingInput> {
        self.document.pending_inputs()
    }
    pub(crate) fn submit(
        &mut self,
        input: String,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<oneshot::Receiver<Result<(), String>>> {
        if self.session.is_archived() || input.trim().is_empty() {
            return None;
        }
        let mode = if self.status == SessionConnectionStatus::Running {
            SubmitMode::Queue
        } else {
            SubmitMode::Start
        };
        let received = self.send(
            SessionCommand::Submit {
                id: InputId::random(),
                text: input,
                mode,
            },
            cx,
        )?;
        Some(unit_reply(received, cx))
    }
    pub(crate) fn submit_edit(
        &mut self,
        input: String,
        target: InputId,
        revision: u64,
        head: Option<u64>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<oneshot::Receiver<Result<(), String>>> {
        if !self.can_branch() || input.trim().is_empty() {
            return None;
        }
        let received = self.send(
            SessionCommand::Edit {
                id: InputId::random(),
                text: input,
                target,
                revision,
                head,
            },
            cx,
        )?;
        Some(unit_reply(received, cx))
    }
    pub(crate) fn resume_pending(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.is_active() && !self.pending_inputs().is_empty() {
            self.send(SessionCommand::ResumePending, cx);
        }
    }
    pub(crate) fn change_pending(
        &mut self,
        input_id: InputId,
        prioritize: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<oneshot::Receiver<Result<(), String>>> {
        if self.input_action_pending {
            return None;
        }
        let command = if prioritize {
            SessionCommand::Prioritize(input_id)
        } else {
            SessionCommand::CancelInput(input_id)
        };
        let received = self.send(command, cx)?;
        self.input_action_pending = true;
        let (sender, receiver) = oneshot::channel();
        cx.spawn(async move |this, cx| {
            let result = received
                .await
                .unwrap_or_else(|_| Err("session command interrupted".into()))
                .map(|_| ());
            let _ = this.update(cx, |runtime, cx| {
                runtime.input_action_pending = false;
                runtime.input_error = result.as_ref().err().cloned();
                cx.notify();
            });
            let _ = sender.send(result);
        })
        .detach();
        Some(receiver)
    }
    pub(crate) fn fork(
        &mut self,
        anchor: u64,
        target: Option<u64>,
        cx: &mut Context<Self>,
    ) -> Option<oneshot::Receiver<Result<Session, String>>> {
        if !self.can_branch() {
            return None;
        }
        let received = self.send(
            SessionCommand::Fork {
                child: harness::SessionId::new(),
                anchor,
                target,
                revision: self.durable_revision,
                head: self.tree().head(),
            },
            cx,
        )?;
        let (sender, receiver) = oneshot::channel();
        cx.spawn(async move |this, cx| {
            let result = match received.await {
                Ok(Ok(CommandResult::Forked(info))) => Session::open(&info.path)
                    .await
                    .map_err(|error| error.to_string()),
                Ok(Err(error)) => Err(error),
                _ => Err("fork did not return a session".into()),
            };
            let _ = this.update(cx, |runtime, cx| {
                runtime.refresh_fork_children(cx);
            });
            let _ = sender.send(result);
        })
        .detach();
        Some(receiver)
    }
    pub(crate) fn abort(&mut self, cx: &mut Context<Self>) {
        if let Some(run) = self.run_token.clone() {
            self.send(SessionCommand::Stop { run }, cx);
        }
    }
    pub(crate) fn decide(&mut self, call_id: String, allow: bool, cx: &mut Context<Self>) {
        if let Some(run) = self.run_token.clone() {
            self.send(
                SessionCommand::Decide {
                    run,
                    call_id,
                    allow,
                },
                cx,
            );
        }
    }
    fn rebuild_display(&mut self) {
        match self.document.selected_path(self.document.tree.head()) {
            Ok(display) => {
                self.view = Arc::new(SessionView::from_document(
                    &display,
                    &self.session.title,
                    &self.tool_schemas,
                    None,
                ));
                Arc::make_mut(&mut self.view).actual_stats = self.document.stats();
                self.display_document = Some(display);
            }
            Err(error) => self.fail_runtime(error.to_string()),
        }
    }

    fn apply_receipt(&mut self, receipt: CommitReceipt) {
        if receipt.revision <= self.durable_revision {
            return;
        }
        let previous_revision = self.document.revisions().conversation;
        let committed_revision = receipt.revision;
        let committed_at_ms = receipt.committed_at_ms;
        let changed_path = receipt.events.iter().any(|event| {
            matches!(
                event.event,
                SessionEvent::ConversationHeadSelected { .. } | SessionEvent::SessionForked { .. }
            )
        });
        let display_events = self
            .display_document
            .as_ref()
            .map(|_| receipt.events.clone());
        match self.document.apply_batch(receipt.events) {
            Ok(delta) => {
                if self.document.revisions().conversation != previous_revision || changed_path {
                    self.transcript_updates = self.transcript_updates.saturating_add(1);
                }
                if changed_path {
                    self.rebuild_display();
                } else if let Some(display) = &mut self.display_document {
                    match display.apply_batch(display_events.unwrap_or_default()) {
                        Ok(delta) => {
                            self.view = Arc::new(SessionView::after_delta(
                                display,
                                &delta,
                                &self.session.title,
                                &self.tool_schemas,
                                &self.view,
                            ))
                        }
                        Err(error) => {
                            self.fail_runtime(error.to_string());
                            return;
                        }
                    }
                } else {
                    self.view = Arc::new(SessionView::after_delta(
                        &self.document,
                        &delta,
                        &self.session.title,
                        &self.tool_schemas,
                        &self.view,
                    ));
                }
                Arc::make_mut(&mut self.view).actual_stats = self.document.stats();
                self.durable_revision = committed_revision;
                if committed_at_ms >= 0 {
                    let updated_at = millis_to_seconds(committed_at_ms);
                    self.session.updated_at = self.session.updated_at.max(updated_at);
                }
            }
            Err(error) => {
                self.fail_runtime(format!("committed session projection failed: {error}"));
            }
        }
    }

    fn fail_runtime(&mut self, error: impl Into<RunFailure>) {
        self.input_error = Some(error.into().message().to_owned());
    }
    fn refresh_view(&mut self) {
        self.view = Arc::new(SessionView::from_document(
            self.display_document.as_ref().unwrap_or(&self.document),
            &self.session.title,
            &self.tool_schemas,
            Some(&self.view),
        ));
        Arc::make_mut(&mut self.view).actual_stats = self.document.stats();
    }
}

fn millis_to_seconds(millis: i64) -> u64 {
    u64::try_from(millis.max(0)).unwrap_or_default() / 1_000
}
fn unit_reply(
    received: oneshot::Receiver<Result<CommandResult, String>>,
    cx: &mut Context<SessionConnection>,
) -> oneshot::Receiver<Result<(), String>> {
    let (sender, receiver) = oneshot::channel();
    cx.spawn(async move |_, _| {
        let result = received
            .await
            .unwrap_or_else(|_| Err("session command interrupted".into()))
            .map(|_| ());
        let _ = sender.send(result);
    })
    .detach();
    receiver
}

#[cfg(test)]
mod tests {
    use super::*;
    fn runtime() -> SessionConnection {
        SessionConnection::new(
            &harness::Harness::default(),
            SessionSetup::new(
                harness::Model::new("test", "key", "http://localhost", "model", 10_000),
                "test",
                Session::memory(),
                ".",
            ),
            "default".into(),
            PathBuf::from("sessions"),
            SessionDocument::default(),
            SessionConfig::default(),
        )
    }
    #[test]
    fn reopening_selected_linear_prefix_does_not_show_the_physical_tail() {
        use crate::session::document::tests::{fixture, recorded};
        for head in [None, Some(1)] {
            let mut events = fixture();
            events.push(recorded(
                events.len() as u64,
                SessionEvent::ConversationHeadSelected { head },
            ));
            let document = SessionDocument::from_events(events).unwrap();
            assert!(!document.tree.has_branches());
            let expected = document
                .selected_path(head)
                .unwrap()
                .conversation_ids()
                .len();
            let runtime = SessionConnection::new(
                &harness::Harness::default(),
                SessionSetup::new(
                    harness::Model::new("test", "key", "http://localhost", "model", 10_000),
                    "test",
                    Session::memory(),
                    ".",
                ),
                "default".into(),
                PathBuf::from("sessions"),
                document,
                SessionConfig::default(),
            );
            assert_eq!(runtime.view.conversation.messages.len(), expected);
        }
    }
    #[test]
    fn committed_timestamp_is_converted_from_milliseconds_to_session_seconds() {
        let mut runtime = runtime();
        let expected_seconds = runtime.session.updated_at.saturating_add(60);
        let committed_at_ms = i64::try_from(expected_seconds.saturating_mul(1_000)).unwrap();
        let tx_id = harness::TxId::from_raw("timestamp-units");
        runtime.apply_receipt(CommitReceipt {
            session_id: runtime.session.id.clone(),
            tx_id: tx_id.clone(),
            base_revision: 0,
            revision: 1,
            request_digest: "timestamp-units".into(),
            committed_at_ms,
            events: vec![harness::RecordedEvent {
                seq: 0,
                tx_id,
                time: harness::EventTime {
                    wall_time_ms: committed_at_ms,
                    clock_id: "test-clock".into(),
                    monotonic_ns: 0,
                },
                event: harness::SessionEvent::InputSubmitted {
                    input_id: harness::InputId::from_raw("timestamp-input"),
                    input: "timestamp".into(),
                    origin: harness::InputOrigin::Queue,
                },
            }],
        });

        assert_eq!(runtime.session.updated_at, expected_seconds);
    }
}
