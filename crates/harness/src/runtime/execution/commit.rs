use super::*;

impl AgentLoop {
    pub(super) async fn commit_now(
        &mut self,
        events: Vec<SessionEvent>,
        sink: &EventSink,
    ) -> Result<CommitReceipt, AgentError> {
        let observed = events
            .into_iter()
            .map(|event| ObservedEvent::new(self.agent.clock.now(), event))
            .collect();
        self.commit_observed(observed, sink).await
    }

    pub(super) async fn commit_observed(
        &mut self,
        events: Vec<ObservedEvent>,
        sink: &EventSink,
    ) -> Result<CommitReceipt, AgentError> {
        let tx_id = TxId::random();
        let drafts = events
            .into_iter()
            .map(|observed| EventDraft {
                tx_id: tx_id.clone(),
                time: observed.time,
                event: observed.event,
            })
            .collect();
        let planned = self.agent.machine.plan_batch(drafts)?;
        self.commit_planned(planned, sink).await
    }

    pub(super) async fn commit_planned(
        &mut self,
        planned: PlannedBatch,
        sink: &EventSink,
    ) -> Result<CommitReceipt, AgentError> {
        let append =
            AppendTx::from_planned(self.agent.info.id.clone(), self.agent.revision, &planned);
        let store = self.agent.store.clone();
        let writer = self.agent.writer.clone().ok_or_else(|| {
            AgentError::Task("session mutation attempted without writer capability".into())
        })?;
        let attempted = append.clone();
        let result = tokio::task::spawn_blocking(move || store.append(&attempted, &writer))
            .await
            .map_err(|error| AgentError::Task(error.to_string()))?;
        let receipt = match result {
            Ok(receipt) => receipt,
            Err(SessionStoreError::OutcomeUnknown { .. }) => {
                let store = self.agent.store.clone();
                let id = self.agent.info.id.clone();
                let tx_id = append.tx_id.clone();
                tokio::task::spawn_blocking(move || store.resolve(&id, &tx_id))
                    .await
                    .map_err(|error| AgentError::Task(error.to_string()))??
                    .ok_or(SessionStoreError::OutcomeUnknown {
                        tx_id: append.tx_id.clone(),
                    })?
            }
            Err(error) => return Err(error.into()),
        };
        if receipt.events.as_slice() != planned.events()
            || receipt.base_revision != self.agent.revision
            || receipt.revision != self.agent.revision.saturating_add(1)
        {
            return Err(AgentError::Task(
                "session store receipt did not match the planned transaction".into(),
            ));
        }
        self.agent.machine.apply_batch(planned)?;
        self.agent.revision = receipt.revision;
        self.agent.info.updated_at = millis_to_seconds(receipt.committed_at_ms);
        publish(sink, AgentEvent::SessionCommitted(receipt.clone()));
        Ok(receipt)
    }

    pub(super) async fn acquire_writer_and_reload(
        &mut self,
        sink: &EventSink,
    ) -> Result<(), AgentError> {
        let writer = self.agent.acquire_or_clone_writer().await?;
        let store = self.agent.store.clone();
        let id = self.agent.info.id.clone();
        let loaded = tokio::task::spawn_blocking(move || store.load(&id))
            .await
            .map_err(|error| AgentError::Task(error.to_string()))??;
        if loaded.metadata.archived_at_ms.is_some() {
            return Err(SessionError::Invalid("archived session cannot run".into()).into());
        }
        if loaded.metadata.project_id != self.agent.info.project_id {
            return Err(SessionError::Invalid(format!(
                "session moved from project {} to {}",
                self.agent.info.project_id, loaded.metadata.project_id
            ))
            .into());
        }
        if loaded.metadata.revision < self.agent.revision {
            return Err(SessionStoreError::Corrupt(format!(
                "session revision moved backwards from {} to {}",
                self.agent.revision, loaded.metadata.revision
            ))
            .into());
        }
        if loaded.metadata.config != self.agent.session_config {
            return Err(SessionError::Invalid(
                "session configuration changed in another writer; reopen the session".into(),
            )
            .into());
        }
        let current_revision = self.agent.revision;
        let mut catch_up = Vec::new();
        let mut events = Vec::new();
        for transaction in loaded.transactions {
            if transaction.revision > current_revision {
                // The receipt is published after the machine catches up, so only the usually-small
                // unseen suffix needs a second event copy. Historical events move directly into
                // replay instead of doubling the complete journal in memory on every run.
                events.extend(transaction.events.iter().cloned());
                catch_up.push(transaction);
            } else {
                events.extend(transaction.events);
            }
        }
        self.agent.machine = SessionMachine::from_events(&events)?;
        self.agent.revision = loaded.metadata.revision;
        self.agent.info.title = loaded.metadata.title;
        self.agent.info.created_at = millis_to_seconds(loaded.metadata.created_at_ms);
        self.agent.info.updated_at = millis_to_seconds(loaded.metadata.updated_at_ms);
        self.agent.writer = Some(writer);
        // An agent may have stayed idle while another process committed. Publish those already
        // durable receipts in revision order before recovery or any new effect so every UI
        // document catches up through the same committed-event route and never sees a sequence gap.
        for receipt in catch_up {
            publish(sink, AgentEvent::SessionCommitted(receipt));
        }
        Ok(())
    }

    pub(super) async fn recover_interrupted(
        &mut self,
        events: &EventSink,
    ) -> Result<(), AgentError> {
        let Some(planned) = self
            .agent
            .machine
            .plan_recovery(TxId::random(), self.agent.clock.now())?
        else {
            return Ok(());
        };
        self.commit_planned(planned, events).await?;
        Ok(())
    }

    pub(super) async fn terminate_after_error(
        &mut self,
        aborted: bool,
        error: &AgentError,
        events: &EventSink,
    ) -> Result<(), AgentError> {
        let reason = if aborted {
            TurnEndReason::Aborted
        } else if matches!(error, AgentError::MaxTurns(_)) {
            TurnEndReason::MaxTurns
        } else {
            TurnEndReason::Failed
        };
        if let Some(batch) = self.agent.machine.plan_termination(
            reason,
            error.to_string(),
            TxId::random(),
            self.agent.clock.now(),
        )? {
            self.commit_planned(batch, events).await?;
        }
        Ok(())
    }
}
