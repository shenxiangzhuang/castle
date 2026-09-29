use super::*;
use std::path::PathBuf;
use std::sync::Arc;

use crate::model::Model;
use crate::session::store::AppendFailpoint;
use crate::session::{Session, SessionConfig};
use crate::tools::{AgentTool, Env};
use async_openai::types::responses::{FunctionTool, Tool};
use futures_util::future::BoxFuture;
use serde_json::json;
use std::fs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::time::timeout;

fn test_agent() -> AgentLoop {
    let mut agent = Agent::new(
        Model::new("test", "key", "http://127.0.0.1", "test-model", 128_000),
        "test instructions",
        Session::memory(),
        ".",
    );
    agent.writer = Some(agent.store.acquire_writer(&agent.info.id).unwrap());
    AgentLoop::new(agent)
}

#[test]
fn resolved_request_config_uses_the_session_reasoning_effort() {
    let mut agent = test_agent();
    agent.agent.model = agent
        .agent
        .model
        .clone()
        .with_reasoning_efforts(&[ReasoningEffort::Low, ReasoningEffort::High]);
    agent.agent.session_config.model.reasoning_effort = Some(ReasoningEffort::High);

    let resolved = agent.resolved_model_config().unwrap();

    assert_eq!(resolved.model, "test-model");
    assert_eq!(resolved.reasoning_effort, Some(ReasoningEffort::High));
}

#[test]
fn unsupported_session_reasoning_effort_is_rejected() {
    let mut agent = test_agent();
    agent.agent.model = agent
        .agent
        .model
        .clone()
        .with_reasoning_efforts(&[ReasoningEffort::Low]);
    agent.agent.session_config.model.reasoning_effort = Some(ReasoningEffort::High);

    assert!(matches!(
        agent.resolved_model_config(),
        Err(AgentError::UnsupportedReasoningEffort {
            effort: ReasoningEffort::High,
            ..
        })
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn agent_task_panic_is_reported_and_returns_a_reloaded_agent() {
    let agent = Agent::new(
        Model::new("test", "key", "http://127.0.0.1", "test-model", 128_000),
        "test instructions",
        Session::memory(),
        ".",
    );
    let session_id = agent.session_info().id.clone();
    let mut active = spawn(agent, Operation::Panic);

    let mut receipts = Vec::new();
    let failure = loop {
        match active
            .next_event()
            .await
            .expect("panic recovery keeps the event stream open")
        {
            AgentEvent::SessionCommitted(receipt) => receipts.push(receipt),
            AgentEvent::RunFailed(failure) => break failure,
            _ => {}
        }
    };
    assert!(failure.message().contains("injected agent task panic"));
    assert!(failure.message().contains("recovered from storage"));
    assert!(!failure.retryable());
    assert!(
        receipts
            .iter()
            .flat_map(|receipt| &receipt.events)
            .any(|recorded| matches!(
                &recorded.event,
                SessionEvent::RunTerminated {
                    outcome: RunOutcome::Aborted,
                    ..
                }
            ))
    );
    let agent = active
        .finish()
        .await
        .expect("panic remains inside the task");
    assert_eq!(agent.session_info().id, session_id);
    assert!(
        agent
            .machine
            .plan_recovery(TxId::random(), agent.clock.now())
            .expect("recovered machine remains valid")
            .is_none()
    );
}

#[test]
fn wall_clock_jumps_do_not_change_monotonic_event_durations() {
    let before = observed_event_time("clock", Duration::from_millis(10), 10_000);
    let wall_moved_back = observed_event_time("clock", Duration::from_millis(20), 5_000);
    let wall_moved_forward = observed_event_time("clock", Duration::from_millis(35), 500_000);

    assert!(wall_moved_back.wall_time_ms < before.wall_time_ms);
    assert!(wall_moved_forward.wall_time_ms > before.wall_time_ms);
    assert_eq!(wall_moved_back.duration_since(&before), Some(10_000_000));
    assert_eq!(
        wall_moved_forward.duration_since(&wall_moved_back),
        Some(15_000_000)
    );
}

struct DelayTool;

impl AgentTool for DelayTool {
    fn name(&self) -> &str {
        "delay"
    }

    fn schema(&self) -> Tool {
        Tool::Function(FunctionTool {
            name: "delay".into(),
            description: None,
            parameters: Some(json!({"type": "object"})),
            strict: Some(false),
            defer_loading: None,
        })
    }

    fn requires_approval(&self) -> bool {
        false
    }

    fn execute<'a>(
        &'a self,
        call: &'a FunctionToolCall,
        _env: &'a Env,
    ) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            let delay =
                serde_json::from_str::<serde_json::Value>(&call.arguments).unwrap()["delay"]
                    .as_u64()
                    .unwrap();
            tokio::time::sleep(Duration::from_millis(delay)).await;
            ToolResult::ok(call.call_id.clone())
        })
    }
}

struct ApprovalDelayTool;

impl AgentTool for ApprovalDelayTool {
    fn name(&self) -> &str {
        "approval_delay"
    }

    fn schema(&self) -> Tool {
        Tool::Function(FunctionTool {
            name: "approval_delay".into(),
            description: None,
            parameters: Some(json!({"type": "object"})),
            strict: Some(false),
            defer_loading: None,
        })
    }

    fn requires_approval(&self) -> bool {
        true
    }

    fn execute<'a>(
        &'a self,
        call: &'a FunctionToolCall,
        _env: &'a Env,
    ) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            let delay =
                serde_json::from_str::<serde_json::Value>(&call.arguments).unwrap()["delay"]
                    .as_u64()
                    .unwrap();
            tokio::time::sleep(Duration::from_millis(delay)).await;
            ToolResult::ok(call.call_id.clone())
        })
    }
}

fn tool_call(call_id: &str, item_id: &str, delay: u64) -> FunctionToolCall {
    named_tool_call(call_id, item_id, "delay", delay)
}

fn named_tool_call(call_id: &str, item_id: &str, name: &str, delay: u64) -> FunctionToolCall {
    FunctionToolCall {
        arguments: json!({"delay": delay}).to_string(),
        call_id: call_id.into(),
        namespace: None,
        name: name.into(),
        id: Some(item_id.into()),
        status: None,
    }
}

fn temp_directory(label: &str) -> PathBuf {
    let directory =
        std::env::temp_dir().join(format!("castle-agent-v2-{label}-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&directory).unwrap();
    directory
}

pub(crate) async fn read_http_request(socket: &mut TcpStream) {
    let mut request = Vec::new();
    let (body_start, content_length) = loop {
        let mut chunk = [0; 4096];
        let bytes = socket.read(&mut chunk).await.unwrap();
        assert_ne!(bytes, 0);
        request.extend_from_slice(&chunk[..bytes]);
        let Some(header_end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") else {
            continue;
        };
        let headers = String::from_utf8_lossy(&request[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then_some(value.trim())
            })
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap();
        break (header_end + 4, content_length);
    };
    while request.len() < body_start + content_length {
        let mut chunk = [0; 4096];
        let bytes = socket.read(&mut chunk).await.unwrap();
        assert_ne!(bytes, 0);
        request.extend_from_slice(&chunk[..bytes]);
    }
}

pub(crate) async fn write_text_stream_response(
    socket: &mut TcpStream,
    text: &str,
    response_id: &str,
) {
    let body = format!(
        "data: {{\"type\":\"response.output_text.delta\",\"sequence_number\":1,\"item_id\":\"msg_1\",\"output_index\":0,\"content_index\":0,\"delta\":\"{text}\"}}\n\ndata: {{\"type\":\"response.completed\",\"sequence_number\":2,\"response\":{{\"created_at\":0,\"id\":\"{response_id}\",\"model\":\"test-model\",\"object\":\"response\",\"output\":[{{\"type\":\"message\",\"content\":[{{\"type\":\"output_text\",\"annotations\":[],\"text\":\"{text}\"}}],\"id\":\"msg_1\",\"role\":\"assistant\",\"status\":\"completed\"}}],\"status\":\"completed\"}}}}\n\n"
    );
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    socket.write_all(response.as_bytes()).await.unwrap();
}

pub(crate) async fn text_stream_model(text: &'static str) -> (Model, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let (body_start, content_length) = loop {
            let mut chunk = [0; 4096];
            let bytes = socket.read(&mut chunk).await.unwrap();
            assert_ne!(bytes, 0);
            request.extend_from_slice(&chunk[..bytes]);
            let Some(header_end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") else {
                continue;
            };
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then_some(value.trim())
                })
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap();
            break (header_end + 4, content_length);
        };
        while request.len() < body_start + content_length {
            let mut chunk = [0; 4096];
            let bytes = socket.read(&mut chunk).await.unwrap();
            assert_ne!(bytes, 0);
            request.extend_from_slice(&chunk[..bytes]);
        }
        let body = format!(
            "data: {{\"type\":\"response.output_text.delta\",\"sequence_number\":1,\"item_id\":\"msg_1\",\"output_index\":0,\"content_index\":0,\"delta\":\"{text}\"}}\n\ndata: {{\"type\":\"response.completed\",\"sequence_number\":2,\"response\":{{\"created_at\":0,\"id\":\"resp_1\",\"model\":\"test-model\",\"object\":\"response\",\"output\":[{{\"type\":\"message\",\"content\":[{{\"type\":\"output_text\",\"annotations\":[],\"text\":\"{text}\"}}],\"id\":\"msg_1\",\"role\":\"assistant\",\"status\":\"completed\"}}],\"status\":\"completed\"}}}}\n\n"
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    });
    (
        Model::new(
            "test",
            "key",
            format!("http://{address}"),
            "test-model",
            128_000,
        ),
        server,
    )
}

pub(crate) async fn gated_two_request_text_stream_model(
    text: &'static str,
) -> (
    Model,
    oneshot::Receiver<()>,
    oneshot::Sender<()>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (first_request_tx, first_request_rx) = oneshot::channel();
    let (release_first_tx, release_first_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut first, _) = listener.accept().await.unwrap();
        read_http_request(&mut first).await;
        first_request_tx.send(()).unwrap();
        release_first_rx.await.unwrap();
        write_text_stream_response(&mut first, text, "resp_1").await;

        let (mut second, _) = listener.accept().await.unwrap();
        read_http_request(&mut second).await;
        write_text_stream_response(&mut second, text, "resp_2").await;
    });
    (
        Model::new(
            "test",
            "key",
            format!("http://{address}"),
            "test-model",
            128_000,
        ),
        first_request_rx,
        release_first_tx,
        server,
    )
}

async fn prepare_tool_request(
    agent: &mut AgentLoop,
    calls: &[FunctionToolCall],
    events: &EventSink,
) {
    let run_id = RunId::random();
    let turn_id = TurnId::random();
    let step_id = StepId::random();
    let input_id = InputId::random();
    agent
        .commit_now(
            vec![
                SessionEvent::InputSubmitted {
                    input_id: input_id.clone(),
                    input: "run tools".into(),
                    origin: InputOrigin::Initial,
                },
                SessionEvent::RunStarted {
                    run_id: run_id.clone(),
                },
                SessionEvent::TurnStarted {
                    run_id,
                    turn_id: turn_id.clone(),
                },
                SessionEvent::StepStarted {
                    turn_id,
                    step_id: step_id.clone(),
                },
                SessionEvent::InputAttached {
                    input_id,
                    step_id: step_id.clone(),
                    items: user_items("run tools".into()),
                },
            ],
            events,
        )
        .await
        .unwrap();
    let request_id = RequestId::random();
    let tools = agent.agent.tool_schemas();
    agent
        .commit_now(
            vec![
                SessionEvent::RequestSnapshot {
                    request_id: request_id.clone(),
                    step_id,
                    reason: crate::RequestHeaderReason::Initial,
                    model: "test-model".into(),
                    instructions: Some("test instructions".into()),
                    tools,
                    reasoning_effort: None,
                    max_output_tokens: None,
                    session_config: SessionConfig::default(),
                },
                SessionEvent::ModelRequestStarted {
                    request_id: request_id.clone(),
                },
            ],
            events,
        )
        .await
        .unwrap();
    let items = calls
        .iter()
        .cloned()
        .map(OutputItem::FunctionCall)
        .map(InputItem::from)
        .collect();
    let mut completed = vec![SessionEvent::AssistantCompleted {
        request_id: request_id.clone(),
        items,
        response: ResponseInfo {
            id: "response".into(),
            model: "test-model".into(),
            usage: None,
        },
    }];
    completed.extend(calls.iter().map(|call| SessionEvent::ToolCallRequested {
        request_id: request_id.clone(),
        call_id: CallId::from_raw(call.call_id.clone()),
        parent_call_id: None,
    }));
    agent.commit_now(completed, events).await.unwrap();
}

#[tokio::test]
async fn machine_does_not_advance_when_commit_fails_before_sqlite_commit() {
    let mut agent = test_agent();
    agent
        .agent
        .store
        .inject_failpoint(AppendFailpoint::BeforeCommitOnce);
    let (events, _) = mpsc::unbounded_channel();
    let before_seq = agent.agent.machine.next_seq();
    let before_revision = agent.agent.revision;
    let result = agent
        .commit_now(
            vec![SessionEvent::InputSubmitted {
                input_id: InputId::random(),
                input: "hello".into(),
                origin: InputOrigin::Initial,
            }],
            &events,
        )
        .await;
    assert!(matches!(
        result,
        Err(AgentError::Store(SessionStoreError::InjectedBeforeCommit))
    ));
    assert_eq!(agent.agent.machine.next_seq(), before_seq);
    assert_eq!(agent.agent.revision, before_revision);
}

#[tokio::test]
async fn ambiguous_commit_is_resolved_and_applied_exactly_once() {
    let mut agent = test_agent();
    agent
        .agent
        .store
        .inject_failpoint(AppendFailpoint::AfterCommitBeforeReceiptOnce);
    let (events, mut received) = mpsc::unbounded_channel();
    let receipt = agent
        .commit_now(
            vec![SessionEvent::InputSubmitted {
                input_id: InputId::random(),
                input: "hello".into(),
                origin: InputOrigin::Initial,
            }],
            &events,
        )
        .await
        .unwrap();
    assert_eq!(agent.agent.revision, 1);
    assert_eq!(agent.agent.machine.next_seq(), 1);
    assert!(matches!(
        received.recv().await,
        Some(AgentEvent::SessionCommitted(committed)) if committed == receipt
    ));
    assert_eq!(
        agent
            .agent
            .store
            .load(&agent.agent.info.id)
            .unwrap()
            .transactions
            .len(),
        1
    );
}

#[tokio::test]
async fn failed_run_closes_request_step_turn_and_run_in_one_transaction() {
    let mut agent = test_agent();
    let (events, mut received) = mpsc::unbounded_channel();
    let run_id = RunId::random();
    let turn_id = TurnId::random();
    let step_id = StepId::random();
    let input_id = InputId::random();
    let tools = agent.agent.tool_schemas();
    agent
        .commit_now(
            vec![
                SessionEvent::InputSubmitted {
                    input_id: input_id.clone(),
                    input: "hello".into(),
                    origin: InputOrigin::Initial,
                },
                SessionEvent::RunStarted {
                    run_id: run_id.clone(),
                },
                SessionEvent::TurnStarted {
                    run_id,
                    turn_id: turn_id.clone(),
                },
                SessionEvent::StepStarted {
                    turn_id,
                    step_id: step_id.clone(),
                },
                SessionEvent::InputAttached {
                    input_id,
                    step_id: step_id.clone(),
                    items: user_items("hello".into()),
                },
            ],
            &events,
        )
        .await
        .unwrap();
    agent
        .commit_now(
            vec![
                SessionEvent::RequestSnapshot {
                    request_id: "request".into(),
                    step_id,
                    reason: crate::RequestHeaderReason::Initial,
                    model: "test-model".into(),
                    instructions: Some("test instructions".into()),
                    tools,
                    reasoning_effort: None,
                    max_output_tokens: None,
                    session_config: SessionConfig::default(),
                },
                SessionEvent::ModelRequestStarted {
                    request_id: "request".into(),
                },
            ],
            &events,
        )
        .await
        .unwrap();
    while received.try_recv().is_ok() {}

    agent
        .terminate_after_error(
            false,
            &AgentError::ModelResponse("provider failed".into()),
            &events,
        )
        .await
        .unwrap();
    let AgentEvent::SessionCommitted(receipt) = received.recv().await.unwrap() else {
        panic!("terminal transaction must be published");
    };
    assert!(matches!(
        receipt.events.as_slice(),
        [
            crate::RecordedEvent {
                event: SessionEvent::ModelRequestFailed { .. },
                ..
            },
            crate::RecordedEvent {
                event: SessionEvent::StepTerminated {
                    outcome: StepOutcome::Failed,
                    ..
                },
                ..
            },
            crate::RecordedEvent {
                event: SessionEvent::TurnTerminated {
                    reason: TurnEndReason::Failed,
                    ..
                },
                ..
            },
            crate::RecordedEvent {
                event: SessionEvent::RunTerminated {
                    outcome: RunOutcome::Failed,
                    ..
                },
                ..
            }
        ]
    ));
    assert!(agent.agent.machine.active_run().is_none());
}

#[tokio::test]
async fn run_persists_each_effect_intent_and_a_complete_terminal_history() {
    let (model, server) = text_stream_model("hello").await;
    let active = Agent::new(model, "test instructions", Session::memory(), ".").start("hi");
    let agent = active.finish().await.unwrap();
    server.await.unwrap();

    assert!(agent.machine.active_run().is_none());
    let loaded = agent.store.load(&agent.info.id).unwrap();
    let request_transaction = loaded
        .transactions
        .iter()
        .find(|transaction| {
            transaction
                .events
                .iter()
                .any(|event| matches!(event.event, SessionEvent::RequestSnapshot { .. }))
        })
        .unwrap();
    assert!(
        request_transaction
            .events
            .iter()
            .any(|event| { matches!(event.event, SessionEvent::ModelRequestStarted { .. }) })
    );
    assert!(loaded.events().any(|event| {
        matches!(
            event.event,
            SessionEvent::RunTerminated {
                outcome: RunOutcome::Completed,
                ..
            }
        )
    }));
}

#[tokio::test]
async fn conversation_edit_fork_reopen_and_search_are_path_local() {
    let directory = std::env::temp_dir().join(format!("castle-tree-{}", uuid::Uuid::new_v4()));
    let session = Session::create(&directory).await.unwrap();
    let (model, server) = text_stream_model("original-answer").await;
    let mut agent = Agent::new(model, "test instructions", session, ".")
        .start("original-question")
        .finish()
        .await
        .unwrap();
    server.await.unwrap();
    let original_head = agent.machine.tree().head();
    let user = agent
        .machine
        .tree()
        .nodes()
        .find_map(|node| node.input_id().cloned())
        .unwrap();
    let original_events: Vec<_> = agent
        .store
        .load(&agent.info.id)
        .unwrap()
        .events()
        .cloned()
        .collect();
    let revision = agent.session_revision();
    // Failed admission leaves both history and head untouched, and never contacts a provider.
    agent
        .store
        .inject_failpoint(AppendFailpoint::BeforeCommitOnce);
    agent = agent
        .edit_input(
            InputId::random(),
            "failed-edit".into(),
            user.clone(),
            revision,
            original_head,
        )
        .finish()
        .await
        .unwrap();
    assert_eq!(agent.machine.tree().head(), original_head);
    assert_eq!(agent.session_revision(), revision);
    let (model, server) = text_stream_model("edited-answer").await;
    agent.set_model(model);
    agent = agent
        .edit_input(
            InputId::random(),
            "edited-question".into(),
            user,
            revision,
            original_head,
        )
        .finish()
        .await
        .unwrap();
    server.await.unwrap();
    let edited_head = agent.machine.tree().head();
    assert!(agent.machine.tree().has_branches());
    let edited_context = serde_json::to_string(&agent.machine.context()).unwrap();
    assert!(edited_context.contains("edited-question"));
    assert!(!edited_context.contains("original-question"));
    let loaded = agent.store.load(&agent.info.id).unwrap();
    let all: Vec<_> = loaded.events().cloned().collect();
    assert_eq!(&all[..original_events.len()], original_events.as_slice());
    let edit_tx = loaded
        .transactions
        .iter()
        .find(|tx| {
            tx.events
                .iter()
                .any(|e| matches!(e.event, SessionEvent::ConversationHeadSelected { .. }))
        })
        .unwrap();
    assert!(
        edit_tx
            .events
            .iter()
            .any(|e| matches!(e.event, SessionEvent::InputAttached { .. }))
    );
    let catalog = Session::catalog(&directory).unwrap();
    let search = format!("{:?}", catalog.search_values);
    assert!(search.contains("edited-question"));
    assert!(!search.contains("original-question"));
    let child_id = crate::SessionId::new();
    let revision = agent.session_revision();
    agent
        .store
        .inject_failpoint(AppendFailpoint::BeforeCommitOnce);
    let (returned, _, result) = agent
        .fork_session(
            child_id.clone(),
            edited_head.unwrap(),
            edited_head,
            revision,
            edited_head,
        )
        .await;
    agent = returned;
    assert!(result.is_err());
    assert!(
        Session::fork_children(&directory, &agent.info.id)
            .unwrap()
            .is_empty()
    );
    agent
        .store
        .inject_failpoint(AppendFailpoint::AfterCommitBeforeReceiptOnce);
    let (returned, _, result) = agent
        .fork_session(
            child_id.clone(),
            edited_head.unwrap(),
            edited_head,
            revision,
            edited_head,
        )
        .await;
    agent = returned;
    let child = result.unwrap();
    assert_eq!(child.info().title, format!("{} (1)", agent.info.title));
    assert_eq!(
        SessionMachine::from_events(child.events())
            .unwrap()
            .context(),
        agent.machine.context()
    );
    let child_path = child.info().path.clone();
    let (returned, _, repeated) = agent
        .fork_session(
            child_id,
            edited_head.unwrap(),
            edited_head,
            revision,
            edited_head,
        )
        .await;
    agent = returned;
    assert_eq!(repeated.unwrap().info().path, child_path);
    assert_eq!(
        Session::fork_children(&directory, &agent.info.id)
            .unwrap()
            .len(),
        1
    );
    Session::archive(child.info()).unwrap();
    let (returned, _, sibling) = agent
        .fork_session(
            crate::SessionId::new(),
            edited_head.unwrap(),
            edited_head,
            revision,
            edited_head,
        )
        .await;
    agent = returned;
    assert_eq!(
        sibling.unwrap().info().title,
        format!("{} (2)", agent.info.title)
    );
    assert_eq!(agent.machine.tree().head(), edited_head);
    let (returned, _, selected) = agent
        .select_conversation(original_head, revision, edited_head)
        .await;
    agent = returned;
    selected.unwrap();
    assert!(
        serde_json::to_string(&agent.machine.context())
            .unwrap()
            .contains("original-question")
    );
    let reopened = Session::open(&agent.info.path).await.unwrap();
    assert_eq!(
        SessionMachine::from_events(reopened.events())
            .unwrap()
            .tree()
            .head(),
        original_head
    );
    assert_eq!(
        SessionMachine::from_events(reopened.events())
            .unwrap()
            .context(),
        agent.machine.context()
    );
    let source_info = agent.info.clone();
    drop(agent);
    drop(reopened);
    Session::delete(&source_info).unwrap();
    let independent = Session::open(&child_path).await.unwrap();
    assert_eq!(
        serde_json::to_string(
            &SessionMachine::from_events(independent.events())
                .unwrap()
                .context()
        )
        .unwrap(),
        edited_context
    );
    drop(independent);
    drop(child);
    fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn tool_finishes_follow_observation_order_but_results_attach_in_call_order() {
    let mut agent = test_agent();
    agent.agent.tools = vec![Arc::new(DelayTool)];
    let (events, _) = mpsc::unbounded_channel();
    let run_id = RunId::random();
    let turn_id = TurnId::random();
    let step_id = StepId::random();
    let input_id = InputId::random();
    agent
        .commit_now(
            vec![
                SessionEvent::InputSubmitted {
                    input_id: input_id.clone(),
                    input: "run tools".into(),
                    origin: InputOrigin::Initial,
                },
                SessionEvent::RunStarted {
                    run_id: run_id.clone(),
                },
                SessionEvent::TurnStarted {
                    run_id,
                    turn_id: turn_id.clone(),
                },
                SessionEvent::StepStarted {
                    turn_id,
                    step_id: step_id.clone(),
                },
                SessionEvent::InputAttached {
                    input_id,
                    step_id: step_id.clone(),
                    items: user_items("run tools".into()),
                },
            ],
            &events,
        )
        .await
        .unwrap();
    let request_id = RequestId::random();
    let tools = agent.agent.tool_schemas();
    agent
        .commit_now(
            vec![
                SessionEvent::RequestSnapshot {
                    request_id: request_id.clone(),
                    step_id,
                    reason: crate::RequestHeaderReason::Initial,
                    model: "test-model".into(),
                    instructions: Some("test instructions".into()),
                    tools,
                    reasoning_effort: None,
                    max_output_tokens: None,
                    session_config: SessionConfig::default(),
                },
                SessionEvent::ModelRequestStarted {
                    request_id: request_id.clone(),
                },
            ],
            &events,
        )
        .await
        .unwrap();
    let calls = vec![
        tool_call("slow", "item-slow", 35),
        tool_call("fast", "item-fast", 1),
    ];
    let items = calls
        .iter()
        .cloned()
        .map(OutputItem::FunctionCall)
        .map(InputItem::from)
        .collect();
    agent
        .commit_now(
            vec![
                SessionEvent::AssistantCompleted {
                    request_id: request_id.clone(),
                    items,
                    response: ResponseInfo {
                        id: "response".into(),
                        model: "test-model".into(),
                        usage: None,
                    },
                },
                SessionEvent::ToolCallRequested {
                    request_id: request_id.clone(),
                    call_id: "slow".into(),
                    parent_call_id: None,
                },
                SessionEvent::ToolCallRequested {
                    request_id,
                    call_id: "fast".into(),
                    parent_call_id: None,
                },
            ],
            &events,
        )
        .await
        .unwrap();
    let (_command_tx, command_rx) = mpsc::unbounded_channel();
    let (_approval_tx, approval_rx) = mpsc::unbounded_channel();
    let mut channels = RunChannels {
        commands: command_rx,
        approvals: approval_rx,
        cancel: CancellationToken::new(),
    };
    agent
        .execute_tools(&calls, &mut channels, &events)
        .await
        .unwrap();

    let loaded = agent.agent.store.load(&agent.agent.info.id).unwrap();
    let finished = loaded
        .events()
        .filter_map(|event| match &event.event {
            SessionEvent::ToolExecutionFinished { call_id, .. } => {
                Some(call_id.as_str().to_owned())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let attached = loaded
        .events()
        .filter_map(|event| match &event.event {
            SessionEvent::ToolResultAttached { call_id, .. } => Some(call_id.as_str().to_owned()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(finished, ["fast", "slow"]);
    assert_eq!(attached, ["slow", "fast"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_authorizations_keep_individual_observation_times_and_dispatch_after_all() {
    let mut agent = test_agent();
    agent.agent.tools = vec![Arc::new(ApprovalDelayTool), Arc::new(DelayTool)];
    let calls = vec![
        named_tool_call("approved-slow", "item-approved-slow", "approval_delay", 35),
        named_tool_call("approved-fast", "item-approved-fast", "approval_delay", 1),
        named_tool_call("not-required", "item-not-required", "delay", 1),
        named_tool_call("not-found", "item-not-found", "missing", 0),
    ];
    let (events, mut received) = mpsc::unbounded_channel();
    prepare_tool_request(&mut agent, &calls, &events).await;
    while received.try_recv().is_ok() {}

    let (command_tx, command_rx) = mpsc::unbounded_channel();
    let (approval_tx, approval_rx) = mpsc::unbounded_channel();
    let mut channels = RunChannels {
        commands: command_rx,
        approvals: approval_rx,
        cancel: CancellationToken::new(),
    };
    let execution_calls = calls.clone();
    let execution = tokio::spawn(async move {
        agent
            .execute_tools(&execution_calls, &mut channels, &events)
            .await
            .unwrap();
        agent
    });

    let mut requested = Vec::new();
    while requested.len() < 2 {
        let event = timeout(Duration::from_secs(2), received.recv())
            .await
            .expect("approval request must be published")
            .expect("event stream must stay open");
        if let AgentEvent::ApprovalRequired(call) = event {
            requested.push(call.call_id);
        }
    }
    assert_eq!(requested, ["approved-slow", "approved-fast"]);

    tokio::time::sleep(Duration::from_millis(12)).await;
    let (slow_acknowledgement, slow_accepted) = oneshot::channel();
    approval_tx
        .send(ApprovalCommand {
            call_id: "approved-slow".into(),
            allow: true,
            acknowledgement: slow_acknowledgement,
        })
        .unwrap();
    slow_accepted.await.unwrap().unwrap();
    tokio::time::sleep(Duration::from_millis(24)).await;
    let (fast_acknowledgement, fast_accepted) = oneshot::channel();
    approval_tx
        .send(ApprovalCommand {
            call_id: "approved-fast".into(),
            allow: true,
            acknowledgement: fast_acknowledgement,
        })
        .unwrap();
    fast_accepted.await.unwrap().unwrap();
    let agent = timeout(Duration::from_secs(3), execution)
        .await
        .expect("tool execution must settle")
        .unwrap();
    drop(command_tx);
    drop(approval_tx);

    let loaded = agent.agent.store.load(&agent.agent.info.id).unwrap();
    let mut authorizations = HashMap::new();
    let mut starts = HashMap::new();
    let mut attached = Vec::new();
    for recorded in loaded.events() {
        match &recorded.event {
            SessionEvent::ToolAuthorizationResolved { call_id, decision } => {
                authorizations.insert(
                    call_id.as_str().to_owned(),
                    (*decision, recorded.time.clone()),
                );
            }
            SessionEvent::ToolExecutionStarted { call_id } => {
                starts.insert(call_id.as_str().to_owned(), recorded.time.clone());
            }
            SessionEvent::ToolResultAttached { call_id, .. } => {
                attached.push(call_id.as_str().to_owned());
            }
            _ => {}
        }
    }

    assert_eq!(authorizations.len(), calls.len());
    let (slow_decision, slow_authorized) = &authorizations["approved-slow"];
    let (fast_decision, fast_authorized) = &authorizations["approved-fast"];
    let (automatic_decision, automatic_authorized) = &authorizations["not-required"];
    let (missing_decision, missing_authorized) = &authorizations["not-found"];
    assert_eq!(*slow_decision, ToolAuthorizationDecision::Allowed);
    assert_eq!(*fast_decision, ToolAuthorizationDecision::Allowed);
    assert_eq!(*automatic_decision, ToolAuthorizationDecision::NotRequired);
    assert_eq!(*missing_decision, ToolAuthorizationDecision::Unavailable);
    assert_eq!(slow_authorized.clock_id, fast_authorized.clock_id);
    assert_eq!(slow_authorized.clock_id, automatic_authorized.clock_id);
    assert_eq!(slow_authorized.clock_id, missing_authorized.clock_id);
    assert!(slow_authorized.monotonic_ns < fast_authorized.monotonic_ns);
    assert!(slow_authorized.wall_time_ms < fast_authorized.wall_time_ms);
    assert!(automatic_authorized.monotonic_ns <= slow_authorized.monotonic_ns);
    assert!(missing_authorized.monotonic_ns <= slow_authorized.monotonic_ns);

    let last_authorization = authorizations
        .values()
        .map(|(_, time)| time.monotonic_ns)
        .max()
        .unwrap();
    assert_eq!(starts.len(), 3);
    assert!(!starts.contains_key("not-found"));
    for started in starts.values() {
        assert_eq!(started.clock_id, slow_authorized.clock_id);
        assert!(started.monotonic_ns >= last_authorization);
    }
    assert_eq!(
        attached,
        [
            "approved-slow",
            "approved-fast",
            "not-required",
            "not-found"
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn approvals_are_scoped_to_interactive_calls_and_retries_must_match() {
    let mut agent = test_agent();
    agent.agent.tools = vec![Arc::new(ApprovalDelayTool), Arc::new(DelayTool)];
    let calls = vec![
        named_tool_call("interactive-a", "item-a", "approval_delay", 1),
        named_tool_call("interactive-b", "item-b", "approval_delay", 1),
        named_tool_call("automatic", "item-automatic", "delay", 1),
        named_tool_call("unavailable", "item-unavailable", "missing", 0),
    ];
    let (events, mut received) = mpsc::unbounded_channel();
    prepare_tool_request(&mut agent, &calls, &events).await;
    while received.try_recv().is_ok() {}

    let (command_tx, command_rx) = mpsc::unbounded_channel();
    let (approval_tx, approval_rx) = mpsc::unbounded_channel();
    let cancellation = CancellationToken::new();
    let control = RunControl {
        commands: command_tx,
        approvals: approval_tx,
        cancel: cancellation.clone(),
    };
    let mut channels = RunChannels {
        commands: command_rx,
        approvals: approval_rx,
        cancel: cancellation,
    };
    let execution_calls = calls.clone();
    let execution_events = events.clone();
    let execution = tokio::spawn(async move {
        agent
            .execute_tools(&execution_calls, &mut channels, &execution_events)
            .await
            .unwrap();
        agent
    });

    let mut requested = Vec::new();
    while requested.len() < 2 {
        let event = timeout(Duration::from_secs(2), received.recv())
            .await
            .expect("interactive approval requests must arrive")
            .expect("event stream must remain open");
        if let AgentEvent::ApprovalRequired(call) = event {
            requested.push(call.call_id);
        }
    }
    assert_eq!(requested, ["interactive-a", "interactive-b"]);

    control.approve("interactive-a").await.unwrap();
    control
        .approve("interactive-a")
        .await
        .expect("an identical retry is idempotent while the approval phase is open");
    let error = control.deny("interactive-a").await.unwrap_err();
    assert!(error.to_string().contains("already resolved as Allowed"));

    for call_id in ["automatic", "unavailable", "unknown"] {
        let error = control.approve(call_id).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("did not request interactive authorization"),
            "unexpected error for {call_id}: {error}"
        );
    }
    control.deny("interactive-b").await.unwrap();

    let agent = timeout(Duration::from_secs(3), execution)
        .await
        .expect("tool execution must settle")
        .unwrap();
    let authorizations = agent
        .agent
        .store
        .load(&agent.agent.info.id)
        .unwrap()
        .events()
        .filter_map(|event| match &event.event {
            SessionEvent::ToolAuthorizationResolved { call_id, decision }
                if call_id == &CallId::from("interactive-a") =>
            {
                Some(*decision)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(authorizations, [ToolAuthorizationDecision::Allowed]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn approval_retry_is_acknowledged_while_the_only_approved_tool_is_running() {
    let mut agent = test_agent();
    agent.agent.tools = vec![Arc::new(ApprovalDelayTool)];
    let calls = vec![named_tool_call(
        "interactive",
        "item-interactive",
        "approval_delay",
        1_000,
    )];
    let (events, mut received) = mpsc::unbounded_channel();
    prepare_tool_request(&mut agent, &calls, &events).await;
    while received.try_recv().is_ok() {}

    let (command_tx, command_rx) = mpsc::unbounded_channel();
    let (approval_tx, approval_rx) = mpsc::unbounded_channel();
    let cancellation = CancellationToken::new();
    let control = RunControl {
        commands: command_tx,
        approvals: approval_tx,
        cancel: cancellation.clone(),
    };
    let mut channels = RunChannels {
        commands: command_rx,
        approvals: approval_rx,
        cancel: cancellation,
    };
    let execution_calls = calls.clone();
    let execution_events = events.clone();
    let execution = tokio::spawn(async move {
        agent
            .execute_tools(&execution_calls, &mut channels, &execution_events)
            .await
            .unwrap();
        agent
    });

    loop {
        if matches!(
            timeout(Duration::from_secs(2), received.recv())
                .await
                .expect("approval request must arrive"),
            Some(AgentEvent::ApprovalRequired(_))
        ) {
            break;
        }
    }
    control.approve("interactive").await.unwrap();
    loop {
        let event = timeout(Duration::from_secs(2), received.recv())
            .await
            .expect("dispatch receipt must arrive")
            .expect("event stream must remain open");
        if matches!(
            event,
            AgentEvent::SessionCommitted(ref receipt)
                if receipt.events.iter().any(|recorded| matches!(
                    &recorded.event,
                    SessionEvent::ToolDispatchIntended { call_id }
                        if call_id == &CallId::from("interactive")
                ))
        ) {
            break;
        }
    }

    timeout(Duration::from_millis(100), control.approve("interactive"))
        .await
        .expect("an acknowledgement retry must not wait for the tool")
        .expect("the persisted decision makes an identical retry idempotent");
    let conflict = timeout(Duration::from_millis(100), control.deny("interactive"))
        .await
        .expect("a conflicting retry must not wait for the tool")
        .unwrap_err();
    assert!(conflict.to_string().contains("already resolved as Allowed"));

    timeout(Duration::from_secs(3), execution)
        .await
        .expect("tool execution must settle")
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn approved_tool_is_durable_when_another_approval_is_aborted() {
    let mut agent = test_agent();
    agent.agent.tools = vec![Arc::new(ApprovalDelayTool)];
    let calls = vec![
        named_tool_call("approved", "item-approved", "approval_delay", 1),
        named_tool_call("pending", "item-pending", "approval_delay", 1),
    ];
    let (events, mut received) = mpsc::unbounded_channel();
    prepare_tool_request(&mut agent, &calls, &events).await;
    while received.try_recv().is_ok() {}

    let (_command_tx, command_rx) = mpsc::unbounded_channel();
    let (approval_tx, approval_rx) = mpsc::unbounded_channel();
    let cancellation = CancellationToken::new();
    let mut channels = RunChannels {
        commands: command_rx,
        approvals: approval_rx,
        cancel: cancellation.clone(),
    };
    let execution_calls = calls.clone();
    let execution_events = events.clone();
    let execution = tokio::spawn(async move {
        let result = agent
            .execute_tools(&execution_calls, &mut channels, &execution_events)
            .await;
        (agent, result)
    });

    let mut requested = Vec::new();
    while requested.len() < 2 {
        let event = timeout(Duration::from_secs(2), received.recv())
            .await
            .expect("approval requests must arrive")
            .expect("event stream must remain open");
        if let AgentEvent::ApprovalRequired(call) = event {
            requested.push(call.call_id);
        }
    }
    assert_eq!(requested, ["approved", "pending"]);

    let (acknowledgement, accepted) = oneshot::channel();
    approval_tx
        .send(ApprovalCommand {
            call_id: "approved".into(),
            allow: true,
            acknowledgement,
        })
        .unwrap();
    accepted.await.unwrap().unwrap();
    cancellation.cancel();

    let (mut agent, result) = execution.await.unwrap();
    assert!(matches!(result, Err(AgentError::Aborted)));
    agent
        .terminate_after_error(true, &AgentError::Aborted, &events)
        .await
        .unwrap();

    let loaded = agent.agent.store.load(&agent.agent.info.id).unwrap();
    let mut authorizations = HashMap::new();
    let mut statuses = HashMap::new();
    let mut starts = Vec::new();
    for recorded in loaded.events() {
        match &recorded.event {
            SessionEvent::ToolAuthorizationResolved { call_id, decision } => {
                authorizations.insert(call_id.as_str().to_owned(), *decision);
            }
            SessionEvent::ToolExecutionStarted { call_id } => {
                starts.push(call_id.as_str().to_owned());
            }
            SessionEvent::ToolResultAttached {
                call_id, status, ..
            } => {
                statuses.insert(call_id.as_str().to_owned(), *status);
            }
            _ => {}
        }
    }
    assert_eq!(
        authorizations.get("approved"),
        Some(&ToolAuthorizationDecision::Allowed)
    );
    assert_eq!(
        authorizations.get("pending"),
        Some(&ToolAuthorizationDecision::Aborted)
    );
    assert!(starts.is_empty());
    assert_eq!(
        statuses.get("approved"),
        Some(&ToolResultStatus::AbortedBeforeDispatch)
    );
    assert_eq!(
        statuses.get("pending"),
        Some(&ToolResultStatus::AbortedBeforeDispatch)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn active_disk_writer_excludes_a_second_agent_without_blocking_readers() {
    let directory = temp_directory("writer-lifecycle");
    let session_a = Session::create_in_project(&directory, "project-a")
        .await
        .unwrap();
    let info = session_a.info().clone();
    let session_b = Session::open_in_project(&info.path, "project-a")
        .await
        .unwrap();
    let (model, first_request, release_first, server) =
        gated_two_request_text_stream_model("done").await;
    let agent_a = Agent::new(model.clone(), "test instructions", session_a, ".");
    let agent_b = Agent::new(model, "test instructions", session_b, ".");

    let active_a = agent_a.start("first");
    timeout(Duration::from_secs(3), first_request)
        .await
        .expect("first model request must reach the local server")
        .expect("local server must signal the first request");

    let mut active_b = agent_b.start("contender");
    let failure = timeout(Duration::from_secs(2), active_b.next_event())
        .await
        .expect("contending agent must fail promptly")
        .expect("contending agent must publish its failure");
    assert!(
        matches!(failure, AgentEvent::RunFailed(failure) if failure.message().contains("active writer"))
    );
    let mut agent_b = active_b.finish().await.unwrap();

    let error = agent_b
        .rename_session("must not acquire while A is active")
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        AgentError::Store(SessionStoreError::WriterBusy { .. })
    ));

    let snapshot = Session::inspect(&info.path).unwrap();
    assert!(snapshot.recovery_needed());
    assert!(
        snapshot
            .events()
            .iter()
            .any(|event| matches!(event.event, SessionEvent::ModelRequestStarted { .. }))
    );
    assert!(!snapshot.events().iter().any(|event| matches!(
        event.event,
        SessionEvent::ModelRequestFailed { .. } | SessionEvent::RunTerminated { .. }
    )));
    let export = directory.join("while-active.jsonl");
    snapshot.export_jsonl(&export).unwrap();
    assert!(fs::metadata(&export).unwrap().len() > 0);
    drop(snapshot);

    release_first.send(()).unwrap();
    let agent_a = timeout(Duration::from_secs(3), active_a.finish())
        .await
        .expect("first agent must finish")
        .unwrap();
    let first_agent_revision = agent_a.revision;
    let first_agent_next_seq = agent_a.machine.next_seq();

    agent_b
        .rename_session("writer reacquired after A finished")
        .await
        .unwrap();
    let mut active_b = agent_b.start("second");
    let mut receipts = Vec::new();
    loop {
        let event = timeout(Duration::from_secs(3), active_b.next_event())
            .await
            .expect("second agent event stream must make progress")
            .expect("second agent event stream must remain open through completion");
        match event {
            AgentEvent::SessionCommitted(receipt) => receipts.push(receipt),
            AgentEvent::RunFinished(_) => break,
            AgentEvent::RunFailed(error) => {
                panic!("second agent unexpectedly failed: {}", error.message())
            }
            _ => {}
        }
    }
    let agent_b = timeout(Duration::from_secs(3), active_b.finish())
        .await
        .expect("second agent must reload and run after A releases the writer")
        .unwrap();
    server.await.unwrap();

    assert!(receipts.len() > first_agent_revision as usize);
    let mut expected_revision = 1_u64;
    let mut expected_seq = 0_u64;
    for receipt in &receipts {
        assert_eq!(receipt.base_revision, expected_revision - 1);
        assert_eq!(receipt.revision, expected_revision);
        for event in &receipt.events {
            assert_eq!(event.seq, expected_seq);
            expected_seq += 1;
        }
        expected_revision += 1;
    }
    assert_eq!(
        receipts[first_agent_revision as usize - 1].revision,
        first_agent_revision
    );
    assert_eq!(
        receipts[first_agent_revision as usize - 1]
            .events
            .last()
            .unwrap()
            .seq
            + 1,
        first_agent_next_seq
    );
    assert_eq!(
        receipts[first_agent_revision as usize].base_revision,
        first_agent_revision
    );
    assert_eq!(expected_revision, agent_b.revision + 1);
    assert_eq!(expected_seq, agent_b.machine.next_seq());

    let final_snapshot = Session::inspect(&info.path).unwrap();
    let inputs = final_snapshot
        .events()
        .iter()
        .filter_map(|event| match &event.event {
            SessionEvent::InputSubmitted { input, .. } => Some(input.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(inputs, ["first", "second"]);
    assert_eq!(
        final_snapshot
            .events()
            .iter()
            .filter(|event| matches!(
                event.event,
                SessionEvent::RunTerminated {
                    outcome: RunOutcome::Completed,
                    ..
                }
            ))
            .count(),
        2
    );
    assert!(!final_snapshot.events().iter().any(|event| matches!(
        event.event,
        SessionEvent::RunTerminated {
            outcome: RunOutcome::Failed | RunOutcome::Aborted,
            ..
        }
    )));

    drop(final_snapshot);
    drop(agent_a);
    drop(agent_b);
    fs::remove_dir_all(directory).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pending_messages_can_be_prioritized_cancelled_stopped_and_resumed() {
    let (model, first_request, _release, server) =
        gated_two_request_text_stream_model("done").await;
    let agent = Agent::new(model, "test", Session::memory(), ".");
    let store = agent.store.clone();
    let session_id = agent.info.id.clone();
    let active = agent.start("first");
    let control = active.control();
    timeout(Duration::from_secs(3), first_request)
        .await
        .unwrap()
        .unwrap();
    for text in ["next", "urgent", "remove"] {
        control.queue(text).await.unwrap();
    }
    let loaded = store.load(&session_id).unwrap();
    let pending = SessionMachine::from_events(&loaded.events().cloned().collect::<Vec<_>>())
        .unwrap()
        .pending_inputs();
    control
        .prioritize(pending[1].input_id.clone())
        .await
        .unwrap();
    control
        .cancel_input(pending[2].input_id.clone())
        .await
        .unwrap();
    // A stale second click must be rejected without killing the current operation.
    assert!(
        control
            .prioritize(pending[2].input_id.clone())
            .await
            .is_err()
    );
    control.queue("still running").await.unwrap();
    control.abort();
    let mut agent = timeout(Duration::from_secs(3), active.finish())
        .await
        .unwrap()
        .unwrap();
    server.abort();
    assert_eq!(agent.machine.pending_inputs().len(), 3);
    let last_id = agent.machine.pending_inputs()[2].input_id.clone();
    let (updated, receipts, result) = agent.cancel_pending_input(last_id).await;
    result.unwrap();
    assert!(receipts.iter().any(|receipt| {
        receipt
            .events
            .iter()
            .any(|record| matches!(record.event, SessionEvent::InputCancelled { .. }))
    }));
    agent = updated;
    let (model, first_request, release, server) = gated_two_request_text_stream_model("done").await;
    agent.set_model(model);
    let active = agent.resume_pending();
    timeout(Duration::from_secs(3), first_request)
        .await
        .unwrap()
        .unwrap();
    release.send(()).unwrap();
    let agent = timeout(Duration::from_secs(3), active.finish())
        .await
        .unwrap()
        .unwrap();
    server.await.unwrap();
    assert!(agent.machine.pending_inputs().is_empty());
    let loaded = agent.store.load(&session_id).unwrap();
    let attached = loaded
        .events()
        .filter_map(|record| match &record.event {
            SessionEvent::InputAttached { input_id, .. } => Some(input_id.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        &attached[1..],
        &[pending[1].input_id.clone(), pending[0].input_id.clone()]
    );
    assert_eq!(attached.len(), 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queue_is_durably_acknowledged_while_stream_dispatch_waits_for_headers() {
    let session = Session::memory();
    let (model, first_request, release_first, server) =
        gated_two_request_text_stream_model("done").await;
    let agent = Agent::new(model, "test instructions", session, ".");
    let active = agent.start("first");
    let control = active.control();

    timeout(Duration::from_secs(3), first_request)
        .await
        .expect("model request must reach the local server")
        .expect("local server must signal the request before sending headers");
    timeout(Duration::from_millis(100), control.queue("queued"))
        .await
        .expect("queue admission must not wait for response headers")
        .expect("queue admission must commit durably");

    release_first.send(()).unwrap();
    let agent = timeout(Duration::from_secs(3), active.finish())
        .await
        .expect("both model requests must settle")
        .unwrap();
    server.await.unwrap();
    let loaded = agent.store.load(&agent.info.id).unwrap();
    let inputs = loaded
        .events()
        .filter_map(|event| match &event.event {
            SessionEvent::InputSubmitted { input, .. } => Some(input.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(inputs, ["first", "queued"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_agent_rejects_external_config_drift_without_writing_journal() {
    let directory = temp_directory("config-drift");
    let session_a = Session::create_in_project(&directory, "project-a")
        .await
        .unwrap();
    let info = session_a.info().clone();
    let session_b = Session::open_in_project(&info.path, "project-a")
        .await
        .unwrap();
    let model = Model::new("test", "key", "http://127.0.0.1:1", "test-model", 128_000);
    let mut agent_a = Agent::new(model.clone(), "test instructions", session_a, ".");
    let agent_b = Agent::new(model, "test instructions", session_b, ".");
    let changed = SessionConfig {
        model: crate::SessionModelConfig {
            model_id: Some("externally-selected-model".into()),
            reasoning_effort: Some(ReasoningEffort::High),
        },
        allow_all_tools: true,
    };
    agent_a.persist_session_config(&changed).await.unwrap();
    let before = Session::inspect(&info.path).unwrap();
    assert_eq!(before.config(), &changed);
    assert!(before.events().is_empty());
    drop(before);

    let mut active_b = agent_b.start("must not be committed");
    let failure = timeout(Duration::from_secs(2), active_b.next_event())
        .await
        .expect("stale agent must fail promptly")
        .expect("stale agent must publish its failure");
    assert!(matches!(
        failure,
        AgentEvent::RunFailed(failure)
            if failure.message().contains("configuration changed")
                && failure.message().contains("reopen")
    ));
    let agent_b = active_b.finish().await.unwrap();

    let after = Session::inspect(&info.path).unwrap();
    assert_eq!(after.config(), &changed);
    assert!(after.events().is_empty());

    drop(after);
    drop(agent_a);
    drop(agent_b);
    fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn run_control_rejects_empty_inputs_before_admission() {
    let (commands, _) = mpsc::unbounded_channel();
    let (approvals, _) = mpsc::unbounded_channel();
    let control = RunControl {
        commands,
        approvals,
        cancel: CancellationToken::new(),
    };
    assert!(matches!(
        control.queue("  ").await,
        Err(AgentError::EmptyInput)
    ));
    assert!(matches!(
        control.steer("\n").await,
        Err(AgentError::EmptyInput)
    ));
}

#[tokio::test]
async fn run_control_reports_error_when_settlement_drops_an_unacknowledged_input() {
    let (commands, mut received) = mpsc::unbounded_channel();
    let (approvals, _) = mpsc::unbounded_channel();
    let control = RunControl {
        commands,
        approvals,
        cancel: CancellationToken::new(),
    };

    let late = tokio::spawn(async move { control.queue("late input").await });
    let command = received.recv().await.expect("input must reach the owner");
    drop(command);
    drop(received);

    let error = late.await.unwrap().unwrap_err();
    assert!(error.to_string().contains("settled before input admission"));
}
