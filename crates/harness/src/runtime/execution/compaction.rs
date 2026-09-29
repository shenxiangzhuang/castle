use super::*;

impl AgentLoop {
    pub(super) async fn compact_once(
        &mut self,
        force: bool,
        custom_instructions: Option<&str>,
        channels: &mut RunChannels,
        events: &EventSink,
    ) -> Result<(), AgentError> {
        let Some(config) = self.agent.compaction else {
            return if force {
                Err(AgentError::NothingToCompact)
            } else {
                Ok(())
            };
        };
        let tools = self.agent.tool_schemas();
        let tokens_before =
            context_tokens(self.agent.machine.state(), &self.agent.instructions, &tools);
        if !force && !config.needs_compaction(tokens_before) {
            return Ok(());
        }
        let Some(prepared) = prepare_compaction(
            self.agent.machine.state(),
            config.keep_recent_tokens,
            custom_instructions,
        ) else {
            return if force {
                Err(AgentError::NothingToCompact)
            } else {
                Ok(())
            };
        };
        let run_id = self.agent.machine.active_run().cloned().ok_or_else(|| {
            AgentError::Task("automatic compaction requires an active run".into())
        })?;
        let compaction_id = CompactionId::random();
        let model_config = self.resolved_model_config()?;
        self.commit_now(
            vec![SessionEvent::CompactionStarted {
                compaction_id: compaction_id.clone(),
                run_id,
                tokens_before,
                first_kept_id: prepared.first_kept_id,
                model: Some(model_config.model.clone()),
                reasoning_effort: model_config.reasoning_effort,
                max_output_tokens: model_config.max_output_tokens,
            }],
            events,
        )
        .await?;
        let mut builder = CreateResponseArgs::default();
        builder
            .model(model_config.model.clone())
            .instructions(SUMMARY_INSTRUCTIONS)
            .input(prepared.prompt)
            .store(false);
        let mut request = builder.build()?;
        request.reasoning = model_config.reasoning();
        request.max_output_tokens = model_config.max_output_tokens;
        let client = self.agent.model.client.clone();
        let responses = client.responses();
        let response = responses.create(request);
        tokio::pin!(response);
        let result = loop {
            tokio::select! {
                _ = channels.cancel.cancelled() => break Err(AgentError::Aborted),
                command = channels.commands.recv() => {
                    if let Some(command) = command {
                        self.submit_command(command, events).await?;
                    }
                }
                approval = channels.approvals.recv() => {
                    reject_inactive_approval(approval);
                }
                response = &mut response => break response.map_err(AgentError::from),
            }
        };
        // Capture provider settlement before SQLite work so compaction timing excludes commit
        // latency and remains comparable with model request timing.
        let observed_at = self.agent.clock.now();
        let response = match result {
            Ok(response) => response,
            Err(error) => {
                self.commit_observed(
                    vec![ObservedEvent::new(
                        observed_at,
                        SessionEvent::CompactionFinished {
                            compaction_id,
                            outcome: if matches!(error, AgentError::Aborted) {
                                StepOutcome::Aborted
                            } else {
                                StepOutcome::Failed
                            },
                            summary: None,
                            response: None,
                        },
                    )],
                    events,
                )
                .await?;
                return Err(error);
            }
        };
        let summary = response.output_text().unwrap_or_default();
        if summary.trim().is_empty() {
            self.commit_observed(
                vec![ObservedEvent::new(
                    observed_at,
                    SessionEvent::CompactionFinished {
                        compaction_id,
                        outcome: StepOutcome::Failed,
                        summary: None,
                        response: Some(response_info(&response)),
                    },
                )],
                events,
            )
            .await?;
            return Err(AgentError::ModelResponse(
                "compaction returned an empty summary".into(),
            ));
        }
        self.commit_observed(
            vec![ObservedEvent::new(
                observed_at,
                SessionEvent::CompactionFinished {
                    compaction_id,
                    outcome: StepOutcome::Completed,
                    summary: Some(summary),
                    response: Some(response_info(&response)),
                },
            )],
            events,
        )
        .await?;
        Ok(())
    }

    pub(super) async fn run_manual_compaction(
        &mut self,
        instructions: Option<&str>,
        mut channels: RunChannels,
        events: &EventSink,
    ) -> Result<(), AgentError> {
        self.recover_interrupted(events).await?;
        let Some(config) = self.agent.compaction else {
            return Err(AgentError::NothingToCompact);
        };
        let tools = self.agent.tool_schemas();
        let tokens = context_tokens(self.agent.machine.state(), &self.agent.instructions, &tools);
        if prepare_compaction(
            self.agent.machine.state(),
            config.keep_recent_tokens,
            instructions,
        )
        .is_none()
        {
            return Err(AgentError::NothingToCompact);
        }
        let run_id = RunId::random();
        self.commit_now(
            vec![SessionEvent::RunStarted {
                run_id: run_id.clone(),
            }],
            events,
        )
        .await?;
        self.compact_once(true, instructions, &mut channels, events)
            .await?;
        self.commit_now(
            vec![SessionEvent::RunTerminated {
                run_id,
                outcome: RunOutcome::Completed,
                error: None,
            }],
            events,
        )
        .await?;
        publish(
            events,
            AgentEvent::RunFinished(RunSummary {
                output: format!("Compacted {tokens} tokens"),
                response_id: String::new(),
                usage: None,
            }),
        );
        Ok(())
    }
}
