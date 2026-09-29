#[cfg(test)]
use crate::InputOrigin;
#[cfg(test)]
use async_openai::types::responses::EasyInputMessage;
use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::time::Duration;

mod commit;
mod compaction;
mod control;
mod model;
mod tools;

use async_openai::types::responses::{
    CreateResponseArgs, FunctionCallOutputItemParam, FunctionToolCall, InputItem, Item, OutputItem,
    Reasoning, ReasoningEffort as ProviderReasoningEffort, Response, ResponseStreamEvent,
};
use futures_util::{FutureExt, StreamExt};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::context::compaction::{SUMMARY_INSTRUCTIONS, context_tokens, prepare_compaction};
use crate::model::ReasoningEffort;
use crate::runtime::context::Agent;
use crate::session::SessionError;
use crate::session::event::{
    AssistantChunk, CallId, CompactionId, EventDraft, EventTime, InputId, RequestId, ResponseInfo,
    RunId, RunOutcome, SessionEvent, StepId, StepOutcome, ToolAuthorizationDecision,
    ToolExecutionOutcome, ToolResultStatus, TurnEndReason, TurnId, TxId,
};
use crate::session::machine::{PlannedBatch, SessionMachine};
use crate::session::store::{AppendTx, CommitReceipt, SessionStoreError};
use crate::tools::ToolResult;
pub use control::{ActiveAgent, AgentError, AgentEvent, RunControl, RunFailure, RunSummary};
use control::{ApprovalCommand, InputAction, InputCommand, RunChannels};

const STREAM_COMMIT_INTERVAL: Duration = Duration::from_millis(32);

type EventSink = mpsc::UnboundedSender<AgentEvent>;

/// Owns an [`Agent`] exclusively while one operation is active.
struct AgentLoop {
    agent: Agent,
}

#[derive(Clone)]
struct ResolvedModelConfig {
    model: String,
    reasoning_effort: Option<ReasoningEffort>,
    max_output_tokens: Option<u32>,
}

impl ResolvedModelConfig {
    fn reasoning(&self) -> Option<Reasoning> {
        self.reasoning_effort.map(|effort| Reasoning {
            effort: Some(provider_reasoning_effort(effort)),
            summary: None,
        })
    }
}

fn provider_reasoning_effort(effort: ReasoningEffort) -> ProviderReasoningEffort {
    match effort {
        ReasoningEffort::None => ProviderReasoningEffort::None,
        ReasoningEffort::Minimal => ProviderReasoningEffort::Minimal,
        ReasoningEffort::Low => ProviderReasoningEffort::Low,
        ReasoningEffort::Medium => ProviderReasoningEffort::Medium,
        ReasoningEffort::High => ProviderReasoningEffort::High,
        ReasoningEffort::Xhigh => ProviderReasoningEffort::Xhigh,
    }
}

impl AgentLoop {
    fn new(agent: Agent) -> Self {
        Self { agent }
    }

    fn into_agent(self) -> Agent {
        self.agent
    }
}

#[cfg(test)]
pub(in crate::runtime) fn start(agent: Agent, input: String) -> ActiveAgent {
    spawn(agent, Operation::Run(Some((InputId::random(), input))))
}

pub(in crate::runtime) fn start_input(
    agent: Agent,
    input_id: InputId,
    input: String,
) -> ActiveAgent {
    spawn(agent, Operation::Run(Some((input_id, input))))
}

pub(in crate::runtime) fn edit_input(
    agent: Agent,
    id: InputId,
    input: String,
    target: InputId,
    revision: u64,
    head: Option<u64>,
) -> ActiveAgent {
    spawn(
        agent,
        Operation::Edit {
            id,
            input,
            target,
            revision,
            head,
        },
    )
}

#[cfg(test)]
pub(in crate::runtime) async fn select_conversation(
    agent: Agent,
    target: Option<u64>,
    revision: u64,
    head: Option<u64>,
) -> (Agent, Vec<CommitReceipt>, Result<(), AgentError>) {
    let (sink, mut receipts) = mpsc::unbounded_channel();
    let mut owner = AgentLoop::new(agent);
    let result = async {
        owner.acquire_writer_and_reload(&sink).await?;
        owner.check_conversation_revision(revision, head)?;
        owner
            .commit_now(
                vec![SessionEvent::ConversationHeadSelected { head: target }],
                &sink,
            )
            .await?;
        Ok(())
    }
    .await;
    owner.agent.writer = None;
    let mut committed = Vec::new();
    while let Ok(AgentEvent::SessionCommitted(receipt)) = receipts.try_recv() {
        committed.push(receipt);
    }
    (owner.into_agent(), committed, result)
}

pub(in crate::runtime) async fn fork_session(
    agent: Agent,
    child: crate::SessionId,
    anchor: u64,
    target: Option<u64>,
    revision: u64,
    head: Option<u64>,
) -> (
    Agent,
    Vec<CommitReceipt>,
    Result<crate::Session, AgentError>,
) {
    let (sink, mut receipts) = mpsc::unbounded_channel();
    let mut owner = AgentLoop::new(agent);
    let result = async {
        owner.acquire_writer_and_reload(&sink).await?;
        owner.check_conversation_revision(revision, head)?;
        let tree = owner.agent.machine.tree();
        let valid = tree.node(anchor).is_some_and(|node| {
            tree.path(head).contains(&anchor)
                && node.settled
                && node.completed
                && node.safe
                && match node.kind {
                    crate::ConversationNodeKind::User(_) => target == node.parent,
                    crate::ConversationNodeKind::Assistant(_) => target == Some(anchor),
                    _ => false,
                }
        });
        if !valid {
            return Err(AgentError::Task(
                "Choose a completed message on the current path".into(),
            ));
        }
        let origin = crate::ForkOrigin {
            session_id: owner.agent.info.id.clone(),
            revision,
            anchor,
            head: target,
            title: owner.agent.info.title.clone(),
        };
        let events = owner.agent.machine.fork_events(target)?;
        let recorded = crate::RecordedEvent {
            seq: 0,
            tx_id: TxId::random(),
            time: owner.agent.clock.now(),
            event: SessionEvent::SessionForked { origin, events },
        };
        SessionMachine::from_events(std::slice::from_ref(&recorded))?;
        let store = owner.agent.store.clone();
        let writer = owner.agent.acquire_or_clone_writer().await?;
        let source = owner.agent.info.id.clone();
        let directory = owner
            .agent
            .info
            .path
            .parent()
            .ok_or_else(|| AgentError::Task("Save the session before forking".into()))?
            .to_owned();
        let request = crate::session::store::CreateStoredSession {
            id: child.clone(),
            project_id: owner.agent.info.project_id.clone(),
            title: owner.agent.info.title.clone(),
            config: owner.agent.session_config.clone(),
            created_at_ms: recorded.time.wall_time_ms,
        };
        tokio::task::spawn_blocking(move || -> Result<crate::Session, AgentError> {
            let created = store.create_fork(
                request.clone(),
                &source,
                revision,
                recorded.clone(),
                &writer,
            );
            if matches!(
                created,
                Err(crate::SessionStoreError::OutcomeUnknown { .. })
            ) {
                store.create_fork(request, &source, revision, recorded, &writer)?;
            } else {
                created?;
            }
            let loaded = store.load(&child)?;
            Ok(crate::Session::from_loaded(
                store,
                loaded,
                crate::session::locator(&directory, &child, false),
            )?)
        })
        .await
        .map_err(|error| AgentError::Task(error.to_string()))?
    }
    .await;
    owner.agent.writer = None;
    let mut committed = Vec::new();
    while let Ok(AgentEvent::SessionCommitted(receipt)) = receipts.try_recv() {
        committed.push(receipt);
    }
    (owner.into_agent(), committed, result)
}

pub(in crate::runtime) fn resume_pending(agent: Agent) -> ActiveAgent {
    spawn(agent, Operation::Run(None))
}

// Return catch-up receipts even on failure so an idle host can keep its projection current.
pub(in crate::runtime) async fn cancel_pending_input(
    agent: Agent,
    input_id: InputId,
) -> (Agent, Vec<CommitReceipt>, Result<(), AgentError>) {
    let (sink, mut receipts) = mpsc::unbounded_channel();
    let mut owner = AgentLoop::new(agent);
    let result = async {
        owner.acquire_writer_and_reload(&sink).await?;
        owner.recover_interrupted(&sink).await?;
        owner
            .commit_now(vec![SessionEvent::InputCancelled { input_id }], &sink)
            .await?;
        Ok(())
    }
    .await;
    owner.agent.writer = None;
    let mut committed = Vec::new();
    while let Ok(AgentEvent::SessionCommitted(receipt)) = receipts.try_recv() {
        committed.push(receipt);
    }
    (owner.into_agent(), committed, result)
}

pub(in crate::runtime) fn start_compaction(
    agent: Agent,
    instructions: Option<String>,
) -> ActiveAgent {
    spawn(agent, Operation::Compact(instructions))
}

enum Operation {
    Run(Option<(InputId, String)>),
    Edit {
        id: InputId,
        input: String,
        target: InputId,
        revision: u64,
        head: Option<u64>,
    },
    Compact(Option<String>),
    #[cfg(test)]
    Panic,
}

fn spawn(agent: Agent, operation: Operation) -> ActiveAgent {
    let (commands_tx, commands_rx) = mpsc::unbounded_channel();
    let (approvals_tx, approvals_rx) = mpsc::unbounded_channel();
    let (events_tx, events_rx) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();
    let control = RunControl {
        commands: commands_tx,
        approvals: approvals_tx,
        cancel: cancel.clone(),
    };
    let channels = RunChannels {
        commands: commands_rx,
        approvals: approvals_rx,
        cancel,
    };
    let task = tokio::spawn(async move {
        let mut agent_loop = AgentLoop::new(agent);
        let outcome = AssertUnwindSafe(async {
            if let Err(error) = agent_loop.acquire_writer_and_reload(&events_tx).await {
                publish(
                    &events_tx,
                    AgentEvent::RunFailed(RunFailure::from_error(&error)),
                );
                return;
            }
            let result = match operation {
                Operation::Run(input) => agent_loop.run(input, None, channels, &events_tx).await,
                Operation::Edit {
                    id,
                    input,
                    target,
                    revision,
                    head,
                } => {
                    agent_loop
                        .run_edit(id, input, target, revision, head, channels, &events_tx)
                        .await
                }
                Operation::Compact(instructions) => {
                    agent_loop
                        .run_manual_compaction(instructions.as_deref(), channels, &events_tx)
                        .await
                }
                #[cfg(test)]
                Operation::Panic => {
                    agent_loop
                        .commit_now(
                            vec![SessionEvent::RunStarted {
                                run_id: RunId::random(),
                            }],
                            &events_tx,
                        )
                        .await
                        .expect("panic fixture commits a started run");
                    panic!("injected agent task panic");
                }
            };
            if let Err(error) = result {
                let aborted = matches!(error, AgentError::Aborted);
                let cleanup = agent_loop
                    .terminate_after_error(aborted, &error, &events_tx)
                    .await;
                match (aborted, cleanup) {
                    (true, Ok(())) => publish(&events_tx, AgentEvent::RunAborted),
                    (false, Ok(())) => {
                        publish(
                            &events_tx,
                            AgentEvent::RunFailed(RunFailure::from_error(&error)),
                        );
                    }
                    (_, Err(cleanup)) => publish(
                        &events_tx,
                        AgentEvent::RunFailed(RunFailure::new(
                            format!("{}; terminal commit failed: {cleanup}", error),
                            false,
                        )),
                    ),
                }
            }
        })
        .catch_unwind()
        .await;
        if let Err(payload) = outcome {
            let panic = panic_message(payload.as_ref());
            // The durable replay replaces every possibly-partial in-memory mutation, then closes
            // the interrupted lifecycle before ownership returns to the host.
            let recovery = async {
                agent_loop.acquire_writer_and_reload(&events_tx).await?;
                agent_loop.recover_interrupted(&events_tx).await
            }
            .await;
            let failure = match recovery {
                Ok(()) => RunFailure::new(
                    format!("agent task panicked and was recovered from storage: {panic}"),
                    false,
                ),
                Err(error) => RunFailure::new(
                    format!(
                        "agent task panicked: {panic}; could not reload durable session: {error}"
                    ),
                    false,
                ),
            };
            publish(&events_tx, AgentEvent::RunFailed(failure));
        }
        // Returning an idle Agent drops the last run-scoped writer capability. Readers and a
        // future run may now acquire the session; OS ownership is also released on crash.
        agent_loop.agent.writer = None;
        agent_loop.into_agent()
    });
    ActiveAgent {
        control,
        events: events_rx,
        task,
    }
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("unknown panic")
}

impl AgentLoop {
    fn check_conversation_revision(
        &self,
        revision: u64,
        head: Option<u64>,
    ) -> Result<(), AgentError> {
        self.agent.machine.require_quiescent()?;
        if self.agent.revision != revision || self.agent.machine.tree().head() != head {
            return Err(AgentError::Task(
                "Conversation changed; reopen the message before editing".into(),
            ));
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_edit(
        &mut self,
        id: InputId,
        input: String,
        target: InputId,
        revision: u64,
        head: Option<u64>,
        channels: RunChannels,
        events: &EventSink,
    ) -> Result<(), AgentError> {
        self.check_conversation_revision(revision, head)?;
        let tree = self.agent.machine.tree();
        let node = tree
            .input_node(&target)
            .filter(|node| tree.path(head).contains(&node.id))
            .ok_or_else(|| AgentError::Task("Message is not on the active branch".into()))?;
        if input.trim().is_empty() || input == node.text {
            return Err(AgentError::Task(
                "Enter a changed, non-empty message".into(),
            ));
        }
        let parent = node.parent;
        self.run(Some((id, input)), Some(parent), channels, events)
            .await
    }

    async fn run(
        &mut self,
        input: Option<(InputId, String)>,
        selected_head: Option<Option<u64>>,
        mut channels: RunChannels,
        events: &EventSink,
    ) -> Result<(), AgentError> {
        self.recover_interrupted(events).await?;
        if channels.cancel.is_cancelled() {
            return Err(AgentError::Aborted);
        }
        let transition = self.agent.machine.transition(
            ::agent::AgentInput::Start {
                input,
                selected_head,
                run: RunId::random(),
                turn: TurnId::random(),
                step: StepId::random(),
            },
            TxId::random(),
            self.agent.clock.now(),
        )?;
        let (batch, effect) = transition.into_parts();
        self.commit_planned(batch, events).await?;
        let ::agent::AgentEffect::RequestModel { step } = effect else {
            return Err(AgentError::Task("start did not request a model".into()));
        };
        let result = self.run_loop(step, &mut channels, events).await;
        match result {
            Ok(summary) => {
                publish(events, AgentEvent::RunFinished(summary));
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    async fn run_loop(
        &mut self,
        mut step_id: StepId,
        channels: &mut RunChannels,
        events: &EventSink,
    ) -> Result<RunSummary, AgentError> {
        let mut request_count = 0_usize;
        loop {
            if channels.cancel.is_cancelled() {
                return Err(AgentError::Aborted);
            }
            if request_count >= self.agent.max_turns {
                return Err(AgentError::MaxTurns(self.agent.max_turns));
            }
            self.compact_once(false, None, channels, events).await?;
            request_count += 1;

            let response = self.request_model(&step_id, channels, events).await?;
            let calls = response
                .output
                .iter()
                .filter_map(|item| match item {
                    OutputItem::FunctionCall(call) => Some(call.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            let summary = RunSummary {
                output: response.output_text().unwrap_or_default(),
                response_id: response.id.clone(),
                usage: response.usage.clone(),
            };

            if !calls.is_empty() {
                self.execute_tools(&calls, channels, events).await?;
            }
            self.drain_inputs(channels, events).await?;
            if channels.cancel.is_cancelled() {
                return Err(AgentError::Aborted);
            }

            let transition = self.agent.machine.transition(
                ::agent::AgentInput::StepCompleted {
                    had_tools: !calls.is_empty(),
                    next_turn: TurnId::random(),
                    next_step: StepId::random(),
                },
                TxId::random(),
                self.agent.clock.now(),
            )?;
            let (batch, effect) = transition.into_parts();
            self.commit_planned(batch, events).await?;
            match effect {
                ::agent::AgentEffect::RequestModel { step } => step_id = step,
                ::agent::AgentEffect::Finished => return Ok(summary),
            }
        }
    }

    async fn drain_inputs(
        &mut self,
        channels: &mut RunChannels,
        events: &EventSink,
    ) -> Result<(), AgentError> {
        while let Ok(command) = channels.commands.try_recv() {
            self.submit_command(command, events).await?;
        }
        Ok(())
    }

    async fn submit_command(
        &mut self,
        command: InputCommand,
        events: &EventSink,
    ) -> Result<(), AgentError> {
        let InputCommand {
            action,
            acknowledgement,
        } = command;
        let event = match action {
            InputAction::Event(event) => *event,
            InputAction::Permission(allow) => {
                let mut config = self.agent.session_config.clone();
                config.allow_all_tools = allow;
                let result = self.agent.persist_session_config(&config).await;
                if result.is_ok() {
                    publish(events, AgentEvent::ConfigChanged(config));
                }
                let _ =
                    acknowledgement.send(result.as_ref().map(|_| ()).map_err(ToString::to_string));
                return result;
            }
        };
        let committed = self.commit_now(vec![event], events).await;
        match committed {
            Ok(_) => {
                let _ = acknowledgement.send(Ok(()));
                Ok(())
            }
            Err(error) => {
                let rejected = matches!(error, AgentError::Machine(_));
                let _ = acknowledgement.send(Err(if rejected {
                    "This message has already started or was removed".into()
                } else {
                    error.to_string()
                }));
                // A stale UI action loses to attachment/cancellation; it must not abort the run.
                if rejected { Ok(()) } else { Err(error) }
            }
        }
    }
}

#[cfg(test)]
fn observed_event_time(clock_id: &str, elapsed: Duration, wall_time_ms: i64) -> EventTime {
    EventTime {
        wall_time_ms,
        clock_id: clock_id.to_owned(),
        monotonic_ns: u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX),
    }
}

struct ObservedEvent {
    time: EventTime,
    event: SessionEvent,
}

impl ObservedEvent {
    fn new(time: EventTime, event: SessionEvent) -> Self {
        Self { time, event }
    }
}

enum ToolTaskEvent {
    Started {
        index: usize,
        time: EventTime,
    },
    Finished {
        index: usize,
        time: EventTime,
        result: ToolResult,
    },
}

#[derive(Clone)]
struct ToolCompletion {
    result: ToolResult,
    status: ToolResultStatus,
}

fn requested_authorization(allow: bool) -> ToolAuthorizationDecision {
    if allow {
        ToolAuthorizationDecision::Allowed
    } else {
        ToolAuthorizationDecision::Denied
    }
}

fn acknowledge_persisted_approval(
    approval: Option<ApprovalCommand>,
    interactive_indexes: &HashMap<String, usize>,
    decisions: &[Option<(ToolAuthorizationDecision, EventTime)>],
) {
    let Some(ApprovalCommand {
        call_id,
        allow,
        acknowledgement,
    }) = approval
    else {
        return;
    };
    let result = interactive_indexes
        .get(&call_id)
        .and_then(|index| decisions.get(*index))
        .and_then(Option::as_ref)
        .map_or_else(
            || Err(format!("tool call {call_id} is not awaiting authorization")),
            |(persisted, _)| {
                let requested = requested_authorization(allow);
                if *persisted == requested {
                    Ok(())
                } else {
                    Err(format!(
                        "tool call {call_id} authorization is already resolved as {persisted:?}"
                    ))
                }
            },
        );
    let _ = acknowledgement.send(result);
}

fn reject_inactive_approval(approval: Option<ApprovalCommand>) {
    let Some(ApprovalCommand {
        call_id,
        acknowledgement,
        ..
    }) = approval
    else {
        return;
    };
    let _ = acknowledgement.send(Err(format!(
        "tool call {call_id} is not awaiting authorization"
    )));
}

#[cfg(test)]
fn user_items(message: String) -> Vec<InputItem> {
    vec![InputItem::from(EasyInputMessage::from(message))]
}

fn function_output(call: &FunctionToolCall, output: String) -> InputItem {
    InputItem::from(Item::from(FunctionCallOutputItemParam {
        call_id: call.call_id.clone(),
        output: output.into(),
        id: None,
        status: None,
    }))
}

fn response_info(response: &Response) -> ResponseInfo {
    ResponseInfo {
        id: response.id.clone(),
        model: response.model.clone(),
        usage: response
            .usage
            .as_ref()
            .map(crate::session::event::TokenUsage::from_provider),
    }
}

fn publish(sink: &EventSink, event: AgentEvent) {
    let _ = sink.send(event);
}

fn millis_to_seconds(millis: i64) -> u64 {
    u64::try_from(millis.max(0)).unwrap_or_default() / 1_000
}

#[cfg(test)]
pub(super) mod tests;
