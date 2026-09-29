use super::*;

impl AgentLoop {
    pub(super) async fn request_model(
        &mut self,
        step_id: &StepId,
        channels: &mut RunChannels,
        events: &EventSink,
    ) -> Result<Response, AgentError> {
        let request_id = RequestId::random();
        let tools = self.agent.tool_schemas();
        let instructions =
            (!self.agent.instructions.is_empty()).then(|| self.agent.instructions.clone());
        let model_config = self.resolved_model_config()?;
        let reasoning_effort = model_config.reasoning_effort;
        let reason = self.agent.machine.expected_request_reason(
            &model_config.model,
            instructions.as_deref(),
            &tools,
            reasoning_effort,
            model_config.max_output_tokens,
            &self.agent.session_config,
        );
        let context = self.agent.machine.context();
        let mut builder = CreateResponseArgs::default();
        builder
            .model(model_config.model.clone())
            .input(context)
            .tools(tools.clone())
            .store(false);
        if let Some(instructions) = instructions.clone() {
            builder.instructions(instructions);
        }
        // Request construction is pure and happens before the durable "started" intent. Once that
        // intent is committed, every exit path below can explicitly close this request.
        let mut request = builder.build()?;
        request.reasoning = model_config.reasoning();
        request.max_output_tokens = model_config.max_output_tokens;
        self.commit_now(
            vec![
                SessionEvent::RequestSnapshot {
                    request_id: request_id.clone(),
                    step_id: step_id.clone(),
                    reason,
                    model: model_config.model,
                    instructions: instructions.clone(),
                    tools: tools.clone(),
                    reasoning_effort,
                    max_output_tokens: model_config.max_output_tokens,
                    session_config: self.agent.session_config.clone(),
                },
                SessionEvent::ModelRequestStarted {
                    request_id: request_id.clone(),
                },
            ],
            events,
        )
        .await?;

        let client = self.agent.model.client.clone();
        let responses = client.responses();
        let stream_request = responses.create_stream(request);
        tokio::pin!(stream_request);
        let stream = loop {
            tokio::select! {
                _ = channels.cancel.cancelled() => {
                    self.fail_request(&request_id, "request cancelled before dispatch", events).await?;
                    return Err(AgentError::Aborted);
                }
                approval = channels.approvals.recv() => {
                    reject_inactive_approval(approval);
                }
                command = channels.commands.recv() => {
                    if let Some(command) = command {
                        self.submit_command(command, events).await?;
                    }
                }
                result = &mut stream_request => break result,
            }
        };
        let mut stream = match stream {
            Ok(stream) => stream,
            Err(error) => {
                self.fail_request(&request_id, &error.to_string(), events)
                    .await?;
                return Err(error.into());
            }
        };

        let mut buffered = Vec::new();
        let mut item_calls = HashMap::<String, CallId>::new();
        let mut interval = tokio::time::interval(STREAM_COMMIT_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        interval.tick().await;
        loop {
            let streamed = tokio::select! {
                _ = channels.cancel.cancelled() => {
                    self.flush_chunks(&request_id, &mut buffered, events).await?;
                    self.fail_request(&request_id, "request cancelled while streaming", events).await?;
                    return Err(AgentError::Aborted);
                }
                _ = interval.tick() => {
                    self.flush_chunks(&request_id, &mut buffered, events).await?;
                    continue;
                }
                command = channels.commands.recv() => {
                    self.flush_chunks(&request_id, &mut buffered, events).await?;
                    if let Some(command) = command {
                        self.submit_command(command, events).await?;
                    }
                    continue;
                }
                approval = channels.approvals.recv() => {
                    reject_inactive_approval(approval);
                    continue;
                }
                event = stream.next() => event,
            };
            let Some(streamed) = streamed else {
                self.flush_chunks(&request_id, &mut buffered, events)
                    .await?;
                self.fail_request(
                    &request_id,
                    "response stream ended before completion",
                    events,
                )
                .await?;
                return Err(AgentError::MissingResponse);
            };
            match streamed {
                Ok(ResponseStreamEvent::ResponseOutputTextDelta(delta)) => {
                    buffered.push(ObservedEvent::new(
                        self.agent.clock.now(),
                        SessionEvent::AssistantChunk {
                            request_id: request_id.clone(),
                            chunk: AssistantChunk::OutputTextDelta { delta: delta.delta },
                        },
                    ))
                }
                Ok(ResponseStreamEvent::ResponseReasoningTextDelta(delta)) => {
                    buffered.push(ObservedEvent::new(
                        self.agent.clock.now(),
                        SessionEvent::AssistantChunk {
                            request_id: request_id.clone(),
                            chunk: AssistantChunk::ReasoningTextDelta { delta: delta.delta },
                        },
                    ))
                }
                Ok(ResponseStreamEvent::ResponseOutputItemAdded(added)) => {
                    if let OutputItem::FunctionCall(call) = added.item {
                        let call_id = CallId::from_raw(call.call_id);
                        if let Some(item_id) = call.id {
                            item_calls.insert(item_id, call_id.clone());
                        }
                        buffered.push(ObservedEvent::new(
                            self.agent.clock.now(),
                            SessionEvent::AssistantChunk {
                                request_id: request_id.clone(),
                                chunk: AssistantChunk::ToolCallDelta {
                                    call_id,
                                    name: Some(call.name),
                                    arguments_delta: String::new(),
                                },
                            },
                        ));
                    }
                }
                Ok(ResponseStreamEvent::ResponseFunctionCallArgumentsDelta(delta)) => {
                    let call_id = item_calls
                        .get(&delta.item_id)
                        .cloned()
                        .unwrap_or_else(|| CallId::from_raw(delta.item_id));
                    buffered.push(ObservedEvent::new(
                        self.agent.clock.now(),
                        SessionEvent::AssistantChunk {
                            request_id: request_id.clone(),
                            chunk: AssistantChunk::ToolCallDelta {
                                call_id,
                                name: None,
                                arguments_delta: delta.delta,
                            },
                        },
                    ));
                }
                Ok(ResponseStreamEvent::ResponseCompleted(completed)) => {
                    let observed_at = self.agent.clock.now();
                    self.flush_chunks(&request_id, &mut buffered, events)
                        .await?;
                    let response = completed.response;
                    if response.output.is_empty() {
                        let error = "model response completed without output items";
                        self.fail_request_observed(&request_id, error, observed_at, events)
                            .await?;
                        return Err(AgentError::ModelResponse(error.into()));
                    }
                    if let Err(error) = self
                        .complete_assistant(&request_id, &response, observed_at.clone(), events)
                        .await
                    {
                        // A provider-shape validation failure leaves the request open because the
                        // assistant transaction was never committed. Close it explicitly before
                        // the run-level terminal transaction.
                        if matches!(error, AgentError::Machine(_)) {
                            self.fail_request_observed(
                                &request_id,
                                &error.to_string(),
                                observed_at,
                                events,
                            )
                            .await?;
                        }
                        return Err(error);
                    }
                    return Ok(response);
                }
                Ok(ResponseStreamEvent::ResponseFailed(failed)) => {
                    let observed_at = self.agent.clock.now();
                    self.flush_chunks(&request_id, &mut buffered, events)
                        .await?;
                    let error = format!("{:?}", failed.response.error);
                    self.fail_request_observed(&request_id, &error, observed_at, events)
                        .await?;
                    return Err(AgentError::ModelResponse(error));
                }
                Ok(ResponseStreamEvent::ResponseIncomplete(incomplete)) => {
                    let observed_at = self.agent.clock.now();
                    self.flush_chunks(&request_id, &mut buffered, events)
                        .await?;
                    let error = format!("incomplete: {:?}", incomplete.response.incomplete_details);
                    self.fail_request_observed(&request_id, &error, observed_at, events)
                        .await?;
                    return Err(AgentError::ModelResponse(error));
                }
                Ok(ResponseStreamEvent::ResponseError(error)) => {
                    let observed_at = self.agent.clock.now();
                    self.flush_chunks(&request_id, &mut buffered, events)
                        .await?;
                    let error = format!("{error:?}");
                    self.fail_request_observed(&request_id, &error, observed_at, events)
                        .await?;
                    return Err(AgentError::ModelResponse(error));
                }
                Ok(_) => {}
                Err(error) => {
                    let observed_at = self.agent.clock.now();
                    self.flush_chunks(&request_id, &mut buffered, events)
                        .await?;
                    self.fail_request_observed(
                        &request_id,
                        &error.to_string(),
                        observed_at,
                        events,
                    )
                    .await?;
                    return Err(error.into());
                }
            }
        }
    }

    pub(super) async fn complete_assistant(
        &mut self,
        request_id: &RequestId,
        response: &Response,
        observed_at: EventTime,
        events: &EventSink,
    ) -> Result<(), AgentError> {
        let items = response
            .output
            .iter()
            .cloned()
            .map(InputItem::from)
            .collect::<Vec<_>>();
        let mut completed = Vec::with_capacity(items.len().saturating_add(1));
        completed.push(SessionEvent::AssistantCompleted {
            request_id: request_id.clone(),
            items,
            response: ResponseInfo {
                id: response.id.clone(),
                model: response.model.clone(),
                usage: response
                    .usage
                    .as_ref()
                    .map(crate::session::event::TokenUsage::from_provider),
            },
        });
        completed.extend(response.output.iter().filter_map(|item| match item {
            OutputItem::FunctionCall(call) => Some(SessionEvent::ToolCallRequested {
                request_id: request_id.clone(),
                call_id: CallId::from_raw(call.call_id.clone()),
                parent_call_id: None,
            }),
            _ => None,
        }));
        self.commit_observed(
            completed
                .into_iter()
                .map(|event| ObservedEvent::new(observed_at.clone(), event))
                .collect(),
            events,
        )
        .await?;
        Ok(())
    }

    pub(super) async fn flush_chunks(
        &mut self,
        request_id: &RequestId,
        buffered: &mut Vec<ObservedEvent>,
        events: &EventSink,
    ) -> Result<(), AgentError> {
        if buffered.is_empty() {
            return Ok(());
        }
        let batch = std::mem::take(buffered);
        match self.commit_observed(batch, events).await {
            Ok(_) => Ok(()),
            Err(error) => {
                // A rejected/pre-commit chunk batch must not strand ModelRequestStarted. If the
                // store is still available, persist the explicit failure and let the caller write
                // the atomic step/turn/run terminal batch.
                let _ = self
                    .fail_request(
                        request_id,
                        &format!("assistant chunk commit failed: {error}"),
                        events,
                    )
                    .await;
                Err(error)
            }
        }
    }

    pub(super) async fn fail_request(
        &mut self,
        request_id: &RequestId,
        error: &str,
        events: &EventSink,
    ) -> Result<(), AgentError> {
        self.fail_request_observed(request_id, error, self.agent.clock.now(), events)
            .await
    }

    pub(super) async fn fail_request_observed(
        &mut self,
        request_id: &RequestId,
        error: &str,
        observed_at: EventTime,
        events: &EventSink,
    ) -> Result<(), AgentError> {
        self.commit_observed(
            vec![ObservedEvent::new(
                observed_at,
                SessionEvent::ModelRequestFailed {
                    request_id: request_id.clone(),
                    error: if error.trim().is_empty() {
                        "model request failed".into()
                    } else {
                        error.to_owned()
                    },
                },
            )],
            events,
        )
        .await?;
        Ok(())
    }

    pub(super) fn resolved_model_config(&self) -> Result<ResolvedModelConfig, AgentError> {
        let reasoning_effort = self.agent.session_config.model.reasoning_effort;
        if let Some(effort) = reasoning_effort
            && !self.agent.model.reasoning_efforts().contains(&effort)
        {
            return Err(AgentError::UnsupportedReasoningEffort {
                effort,
                model: self.agent.model.model.clone(),
            });
        }
        Ok(ResolvedModelConfig {
            model: self.agent.model.model.clone(),
            reasoning_effort,
            max_output_tokens: self.agent.model.max_output_tokens,
        })
    }
}
