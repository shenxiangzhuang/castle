use super::*;

impl AgentLoop {
    #[allow(
        clippy::expect_used,
        reason = "authorization loop resolves every indexed tool before dispatch"
    )]
    pub(super) async fn execute_tools(
        &mut self,
        calls: &[FunctionToolCall],
        channels: &mut RunChannels,
        events: &EventSink,
    ) -> Result<(), AgentError> {
        let mut tasks = JoinSet::new();
        let mut outcomes = vec![None::<ToolCompletion>; calls.len()];
        let mut resolved_tools = Vec::with_capacity(calls.len());
        let mut decisions = Vec::with_capacity(calls.len());
        // Only calls which actually requested an interactive decision are approval capabilities.
        // Keep resolved interactive calls in this map until the approval phase closes so a retry
        // of the same decision can be acknowledged idempotently after an acknowledgement loss.
        let mut interactive_approval_indexes = HashMap::new();
        let mut automatic_authorizations = Vec::with_capacity(calls.len());
        let mut approval_requests = Vec::new();
        for (index, call) in calls.iter().enumerate() {
            let tool = self
                .agent
                .tools
                .iter()
                .find(|tool| tool.name() == call.name)
                .cloned();
            let decision = if tool.is_none() {
                outcomes[index] = Some(ToolCompletion {
                    result: ToolResult::error(format!("Tool not found: {}", call.name)),
                    status: ToolResultStatus::NotFound,
                });
                Some((
                    ToolAuthorizationDecision::Unavailable,
                    self.agent.clock.now(),
                ))
            } else if self.agent.session_config.allow_all_tools
                || tool.as_ref().is_some_and(|tool| !tool.requires_approval())
            {
                Some((
                    ToolAuthorizationDecision::NotRequired,
                    self.agent.clock.now(),
                ))
            } else {
                interactive_approval_indexes.insert(call.call_id.clone(), index);
                approval_requests.push(call.clone());
                None
            };
            if let Some((decision, observed_at)) = &decision {
                automatic_authorizations.push(ObservedEvent::new(
                    observed_at.clone(),
                    SessionEvent::ToolAuthorizationResolved {
                        call_id: CallId::from_raw(call.call_id.clone()),
                        decision: *decision,
                    },
                ));
            }
            resolved_tools.push(tool);
            decisions.push(decision);
        }

        // Authorization is an independently observed fact, not part of dispatch. Persist all
        // automatic decisions before exposing approval prompts, then persist every interactive
        // decision as it arrives. A crash while another call is still awaiting approval therefore
        // cannot erase an earlier decision or its waiting time.
        if !automatic_authorizations.is_empty() {
            self.commit_observed(automatic_authorizations, events)
                .await?;
        }
        for call in approval_requests {
            publish(events, AgentEvent::ApprovalRequired(call));
        }

        while decisions.iter().any(Option::is_none) {
            tokio::select! {
                _ = channels.cancel.cancelled() => return Err(AgentError::Aborted),
                command = channels.commands.recv() => {
                    if let Some(command) = command {
                        self.submit_command(command, events).await?;
                    }
                }
                approval = channels.approvals.recv() => {
                    let Some(ApprovalCommand {
                        call_id,
                        allow,
                        acknowledgement,
                    }) = approval else {
                        return Err(AgentError::Aborted);
                    };
                    let Some(index) = interactive_approval_indexes.get(&call_id).copied() else {
                        let _ = acknowledgement.send(Err(format!(
                            "tool call {call_id} did not request interactive authorization"
                        )));
                        continue;
                    };
                    let requested_decision = requested_authorization(allow);
                    if let Some((persisted_decision, _)) = decisions[index] {
                        let result = if persisted_decision == requested_decision {
                            Ok(())
                        } else {
                            Err(format!(
                                "tool call {call_id} authorization is already resolved as {persisted_decision:?}"
                            ))
                        };
                        let _ = acknowledgement.send(result);
                        continue;
                    }
                    let decision = if requested_decision == ToolAuthorizationDecision::Allowed {
                        requested_decision
                    } else {
                        outcomes[index] = Some(ToolCompletion {
                            result: ToolResult::error("Tool call denied by user"),
                            status: ToolResultStatus::Denied,
                        });
                        requested_decision
                    };
                    let observed_at = self.agent.clock.now();
                    let committed = self
                        .commit_observed(
                            vec![ObservedEvent::new(
                                observed_at.clone(),
                                SessionEvent::ToolAuthorizationResolved {
                                    call_id: CallId::from_raw(call_id),
                                    decision,
                                },
                            )],
                            events,
                        )
                        .await;
                    match committed {
                        Ok(_) => {
                            decisions[index] = Some((decision, observed_at));
                            let _ = acknowledgement.send(Ok(()));
                        }
                        Err(error) => {
                            let _ = acknowledgement.send(Err(error.to_string()));
                            return Err(error);
                        }
                    }
                }
            }
        }

        let dispatch_time = self.agent.clock.now();
        let mut dispatch = Vec::with_capacity(calls.len());
        for (call, resolved) in calls.iter().zip(&decisions) {
            let (decision, _) = resolved
                .as_ref()
                .expect("all tool authorization decisions are resolved");
            let call_id = CallId::from_raw(call.call_id.clone());
            if decision.permits_execution() {
                dispatch.push(ObservedEvent::new(
                    dispatch_time.clone(),
                    SessionEvent::ToolDispatchIntended { call_id },
                ));
            }
        }
        // No tool task exists until the complete dispatch intent is durable. A crash before this
        // receipt has no tool side effects; a crash after it is conservatively recovered as
        // unknown side effects even if the task's actual start observation was not committed yet.
        if !dispatch.is_empty() {
            self.commit_observed(dispatch, events).await?;
        }

        let (task_events, mut task_event_receiver) = mpsc::unbounded_channel();
        for (index, ((call, tool), decision)) in calls
            .iter()
            .zip(resolved_tools)
            .zip(decisions.iter())
            .enumerate()
        {
            let (decision, _) = decision
                .as_ref()
                .expect("all tool authorization decisions are resolved");
            if !decision.permits_execution() {
                continue;
            }
            let tool = tool.expect("permitted tool must be registered");
            let call = call.clone();
            let env = self.agent.env.clone();
            let clock = self.agent.clock.clone();
            let task_events = task_events.clone();
            tasks.spawn(async move {
                let _ = task_events.send(ToolTaskEvent::Started {
                    index,
                    time: clock.now(),
                });
                let result = tool.execute(&call, &env).await;
                let _ = task_events.send(ToolTaskEvent::Finished {
                    index,
                    time: clock.now(),
                    result,
                });
            });
        }
        drop(task_events);

        let mut next_attachment = 0_usize;
        self.attach_ready_results(calls, &mut outcomes, &mut next_attachment, events)
            .await?;
        while next_attachment != calls.len() {
            tokio::select! {
                _ = channels.cancel.cancelled() => return Err(AgentError::Aborted),
                command = channels.commands.recv() => {
                    if let Some(command) = command {
                        self.submit_command(command, events).await?;
                    }
                }
                approval = channels.approvals.recv() => {
                    acknowledge_persisted_approval(
                        approval,
                        &interactive_approval_indexes,
                        &decisions,
                    );
                }
                task_event = task_event_receiver.recv() => {
                    let Some(task_event) = task_event else {
                        return Err(AgentError::Task("tool execution event stream ended early".into()));
                    };
                    match task_event {
                        ToolTaskEvent::Started { index, time } => {
                            self.commit_observed(
                                vec![ObservedEvent::new(
                                    time,
                                    SessionEvent::ToolExecutionStarted {
                                        call_id: CallId::from_raw(calls[index].call_id.clone()),
                                    },
                                )],
                                events,
                            )
                            .await?;
                        }
                        ToolTaskEvent::Finished { index, time, result } => {
                            let status = if result.is_error {
                                ToolResultStatus::Error
                            } else {
                                ToolResultStatus::Success
                            };
                            self.commit_observed(
                                vec![ObservedEvent::new(
                                    time,
                                    SessionEvent::ToolExecutionFinished {
                                        call_id: CallId::from_raw(calls[index].call_id.clone()),
                                        outcome: if result.is_error {
                                            ToolExecutionOutcome::Error
                                        } else {
                                            ToolExecutionOutcome::Success
                                        },
                                    },
                                )],
                                events,
                            )
                            .await?;
                            outcomes[index] = Some(ToolCompletion { result, status });
                            self.attach_ready_results(
                                calls,
                                &mut outcomes,
                                &mut next_attachment,
                                events,
                            )
                            .await?;
                        }
                    }
                }
            }
        }
        while let Some(task) = tasks.join_next().await {
            task.map_err(|error| AgentError::Task(error.to_string()))?;
        }
        self.attach_ready_results(calls, &mut outcomes, &mut next_attachment, events)
            .await?;
        if next_attachment != calls.len() {
            return Err(AgentError::Task(
                "tool results did not become attachable in call order".into(),
            ));
        }
        Ok(())
    }

    pub(super) async fn attach_ready_results(
        &mut self,
        calls: &[FunctionToolCall],
        outcomes: &mut [Option<ToolCompletion>],
        next: &mut usize,
        events: &EventSink,
    ) -> Result<(), AgentError> {
        let mut attached = Vec::new();
        while *next < calls.len() {
            let Some(completion) = outcomes[*next].take() else {
                break;
            };
            let call = &calls[*next];
            attached.push(SessionEvent::ToolResultAttached {
                call_id: CallId::from_raw(call.call_id.clone()),
                status: completion.status,
                item: function_output(call, completion.result.output),
            });
            *next += 1;
        }
        if !attached.is_empty() {
            self.commit_now(attached, events).await?;
        }
        Ok(())
    }
}
