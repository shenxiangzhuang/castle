use super::*;
use crate::runtime::execution::tests::{
    read_http_request, text_stream_model, write_text_stream_response,
};
use crate::session::store::{SessionStore, SessionStoreError};
use std::time::Duration;

fn model() -> Model {
    Model::new(
        "test",
        "secret",
        "http://127.0.0.1:1",
        "test-model",
        128_000,
    )
}
fn attach(
    harness: &Harness,
    session: Session,
    config: SessionConfig,
    model: Model,
) -> SessionHandle {
    harness.attach(
        SessionSetup::new(model, "test", session, "."),
        "project".into(),
        None,
        config,
    )
}
async fn until(
    host: &SessionHandle,
    condition: impl Fn(&RuntimeSnapshot) -> bool,
) -> RuntimeSnapshot {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut connection = host.connect().unwrap();
        if condition(&connection.snapshot) {
            return connection.snapshot;
        }
        loop {
            if let SessionUpdate::Changed(snapshot) = connection.events.recv().await.unwrap()
                && condition(&snapshot)
            {
                return *snapshot;
            }
        }
    })
    .await
    .expect("session did not reach the expected state")
}
fn submit(id: InputId) -> SessionCommand {
    SessionCommand::Submit {
        id,
        text: "hello".into(),
        mode: SubmitMode::Start,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn public_client_commits_reconnects_and_retries_after_reopen_without_running_twice() {
    let root = std::env::temp_dir().join(format!("harness-command-{}", SessionId::new()));
    let config = SessionConfig::default();
    let session = Session::create_in_project_with_config(&root, "project", config.clone())
        .await
        .unwrap();
    let path = session.info().path.clone();
    let (model, server) = text_stream_model("done").await;
    let harness = Harness::default();
    let host = attach(&harness, session, config.clone(), model.clone());
    let command = crate::TxId::random();
    let input = InputId::random();
    let observer = host.connect().unwrap();
    drop(observer); // disconnect does not cancel execution
    host.send_with_id(command.clone(), submit(input.clone()))
        .await
        .unwrap();
    let snapshot = until(&host, |s| s.completed_runs == 1 && !s.status.is_active()).await;
    assert_eq!(snapshot.summary.as_ref().unwrap().output, "done");
    assert_eq!(
        snapshot
            .events
            .iter()
            .filter(|record| matches!(record.event, crate::SessionEvent::InputSubmitted { .. }))
            .count(),
        1
    );
    assert!(snapshot.can_branch);
    server.await.unwrap();
    harness.shutdown().await;
    drop(host);
    let harness = Harness::default();
    let host = attach(&harness, Session::open(&path).await.unwrap(), config, model);
    host.send_with_id(command.clone(), submit(input))
        .await
        .unwrap();
    assert_eq!(host.snapshot().unwrap().revision, snapshot.revision);
    assert!(!host.snapshot().unwrap().status.is_active());
    assert!(
        host.send_with_id(command, SessionCommand::Rename("different".into()))
            .await
            .unwrap_err()
            .contains("different content")
    );
    harness.shutdown().await;
    drop(host);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn configuration_is_published_after_persistence_and_old_retry_cannot_overwrite_newer_config()
{
    let harness = Harness::default();
    let host = attach(
        &harness,
        Session::memory(),
        SessionConfig::default(),
        model(),
    );
    let id = crate::TxId::random();
    let config = SessionConfig {
        allow_all_tools: true,
        ..Default::default()
    };
    host.send_with_id(
        id.clone(),
        SessionCommand::Configure {
            config: config.clone(),
            model: None,
        },
    )
    .await
    .unwrap();
    assert!(host.snapshot().unwrap().config.allow_all_tools);
    host.send(SessionCommand::Configure {
        config: SessionConfig::default(),
        model: None,
    })
    .await
    .unwrap();
    host.send_with_id(
        id,
        SessionCommand::Configure {
            config,
            model: None,
        },
    )
    .await
    .unwrap();
    assert!(!host.snapshot().unwrap().config.allow_all_tools);
    host.send(SessionCommand::Rename("title".into()))
        .await
        .unwrap();
    assert_eq!(host.snapshot().unwrap().session.title, "title");
    harness.shutdown().await;
}

#[test]
fn unfinished_command_reservation_is_explicitly_unknown_and_does_not_reexecute() {
    let root = std::env::temp_dir().join(format!("harness-reservation-{}", SessionId::new()));
    let session = SessionId::new();
    let id = crate::TxId::random();
    let store = SessionStore::open_project(&root).unwrap();
    assert!(
        store
            .reserve_command(&session, &id, b"request")
            .unwrap()
            .is_none()
    );
    drop(store);
    let store = SessionStore::open_project(&root).unwrap();
    assert!(matches!(
        store.reserve_command(&session, &id, b"request"),
        Err(SessionStoreError::OutcomeUnknown { .. })
    ));
    assert!(store.reserve_command(&session, &id, b"different").is_err());
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn slow_observer_gets_explicit_lag_and_reconnects_to_latest_config() {
    let harness = Harness::default();
    let host = attach(
        &harness,
        Session::memory(),
        SessionConfig::default(),
        model(),
    );
    let mut observer = host.connect().unwrap();
    for i in 0..CAPACITY {
        host.send(SessionCommand::Rename(format!("title-{i}")))
            .await
            .unwrap();
    }
    assert!(matches!(
        observer.events.recv().await,
        Err(broadcast::error::RecvError::Lagged(_))
    ));
    let mut connection = host.connect().unwrap();
    assert_eq!(
        connection.snapshot.session.title,
        format!("title-{}", CAPACITY - 1)
    );
    host.send(SessionCommand::Rename("last".into()))
        .await
        .unwrap();
    let mut saw_last = false;
    while let Ok(event) = connection.events.try_recv() {
        if let SessionUpdate::Changed(snapshot) = event {
            saw_last |= snapshot.session.title == "last";
        }
    }
    assert!(saw_last);
    harness.shutdown().await;
}

// The server asks for an interactive tool. Cancelling or changing policy exercises the
// entire public path through request commit, authorization, execution and settlement.
async fn tool_model() -> (Model, tokio::task::JoinHandle<()>) {
    use tokio::io::AsyncWriteExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_http_request(&mut socket).await;
        let event = serde_json::json!({"type":"response.completed","sequence_number":1,"response":{"id":"tools","created_at":0,"model":"test-model","object":"response","status":"completed","output":[{"type":"function_call","call_id":"call","id":"tool","name":"shell","arguments":"{\"command\":\"echo hello\"}"}]}});
        let body = format!("data: {event}\n\n");
        socket.write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        let (mut socket, _) = listener.accept().await.unwrap();
        read_http_request(&mut socket).await;
        write_text_stream_response(&mut socket, "done", "done").await;
    });
    (
        Model::new(
            "test",
            "key",
            format!("http://{address}"),
            "test-model",
            128_000,
        ),
        task,
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn permission_change_resolves_waiting_approval_and_is_durable_before_dispatch() {
    let harness = Harness::default();
    let (model, server) = tool_model().await;
    let host = attach(&harness, Session::memory(), SessionConfig::default(), model);
    host.send(submit(InputId::random())).await.unwrap();
    until(&host, |s| !s.approvals.is_empty()).await;
    assert!(
        host.send(SessionCommand::Decide {
            run: crate::RunId::random(),
            call_id: "call".into(),
            allow: true
        })
        .await
        .is_err()
    );
    host.send(SessionCommand::Configure {
        config: SessionConfig {
            allow_all_tools: true,
            ..Default::default()
        },
        model: None,
    })
    .await
    .unwrap();
    let done = until(&host, |s| s.completed_runs == 1 && !s.status.is_active()).await;
    assert!(done.config.allow_all_tools);
    assert!(done.approvals.is_empty());
    assert!(done.events.iter().any(|record| matches!(
        record.event,
        crate::SessionEvent::ToolDispatchIntended { .. }
    )));
    server.await.unwrap();
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_joins_run_releases_writer_and_closes_subscriptions() {
    let harness = Harness::default();
    let (model, server) = tool_model().await;
    let session = Session::memory();
    let id = session.info().id.clone();
    let setup = SessionSetup::new(model, "test", session, ".");
    let store = setup.agent.store.clone();
    let host = harness.attach(setup, "project".into(), None, SessionConfig::default());
    host.send(submit(InputId::random())).await.unwrap();
    until(&host, |s| !s.approvals.is_empty()).await;
    assert!(store.acquire_writer(&id).is_err());
    let mut observer = host.connect().unwrap();
    tokio::time::timeout(Duration::from_secs(5), harness.shutdown())
        .await
        .unwrap();
    assert!(!host.snapshot().unwrap().status.is_active());
    assert!(
        host.connect().is_err(),
        "shutdown must close future subscriptions"
    );
    while observer.events.try_recv().is_ok() {}
    assert!(matches!(
        observer.events.try_recv(),
        Err(broadcast::error::TryRecvError::Closed)
    ));
    assert!(store.acquire_writer(&id).is_ok());
    assert!(
        host.send(SessionCommand::Rename("after shutdown".into()))
            .await
            .is_err()
    );
    server.abort();
}

#[tokio::test]
async fn rejected_configuration_preserves_previous_model_and_policy() {
    let harness = Harness::default();
    let setup = SessionSetup::new(model(), "test", Session::memory(), ".");
    let store = setup.agent.store.clone();
    let id = setup.agent.info.id.clone();
    let writer = store.acquire_writer(&id).unwrap();
    let host = harness.attach(setup, "project".into(), None, SessionConfig::default());
    let requested = SessionConfig {
        allow_all_tools: true,
        ..Default::default()
    };
    assert!(
        host.send(SessionCommand::Configure {
            config: requested,
            model: Some(model())
        })
        .await
        .is_err()
    );
    assert_eq!(host.snapshot().unwrap().config, SessionConfig::default());
    assert_eq!(
        store.metadata(&id).unwrap().config,
        SessionConfig::default()
    );
    assert_eq!(host.snapshot().unwrap().status, RuntimeStatus::Idle);
    drop(writer);
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_preserves_pending_input_and_resume_requires_an_explicit_command() {
    let harness = Harness::default();
    let (model, server) = tool_model().await;
    let host = attach(&harness, Session::memory(), SessionConfig::default(), model);
    host.send(submit(InputId::random())).await.unwrap();
    let waiting = until(&host, |s| !s.approvals.is_empty()).await;
    let pending = InputId::random();
    host.send(SessionCommand::Submit {
        id: pending.clone(),
        text: "queued".into(),
        mode: SubmitMode::Queue,
    })
    .await
    .unwrap();
    host.send(SessionCommand::Stop { run: waiting.run })
        .await
        .unwrap();
    let snapshot = host.snapshot().unwrap();
    assert_eq!(snapshot.status, RuntimeStatus::Idle);
    let core =
        ::agent::SessionMachine::from_events(&snapshot.events.iter().cloned().collect::<Vec<_>>())
            .unwrap();
    assert_eq!(core.pending_inputs()[0].input_id, pending);
    host.send(SessionCommand::ResumePending).await.unwrap();
    let snapshot = host.snapshot().unwrap();
    assert_eq!(snapshot.completed_runs, 1);
    assert!(!snapshot.status.is_active());
    let core =
        ::agent::SessionMachine::from_events(&snapshot.events.iter().cloned().collect::<Vec<_>>())
            .unwrap();
    assert!(core.pending_inputs().is_empty());
    server.await.unwrap();
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_live_policy_write_cannot_authorize_waiting_tools() {
    let root = std::env::temp_dir().join(format!("harness-policy-failure-{}", SessionId::new()));
    let session = Session::create_in_project(&root, "project").await.unwrap();
    let harness = Harness::default();
    let (model, server) = tool_model().await;
    let host = attach(&harness, session, SessionConfig::default(), model);
    host.send(submit(InputId::random())).await.unwrap();
    until(&host, |s| !s.approvals.is_empty()).await;
    {
        let connection =
            rusqlite::Connection::open(root.join(crate::SESSION_DATABASE_FILE)).unwrap();
        connection.execute_batch("CREATE TRIGGER reject_config BEFORE UPDATE OF config_json ON sessions BEGIN SELECT RAISE(ABORT, 'injected policy write failure'); END;").unwrap();
    }
    assert!(
        host.send(SessionCommand::Configure {
            config: SessionConfig {
                allow_all_tools: true,
                ..Default::default()
            },
            model: None,
        })
        .await
        .is_err()
    );
    let failed = until(&host, |s| matches!(s.status, RuntimeStatus::Failed(_))).await;
    assert!(!failed.config.allow_all_tools);
    assert!(!failed.events.iter().any(|record| matches!(
        record.event,
        crate::SessionEvent::ToolDispatchIntended { .. }
    )));
    harness.shutdown().await;
    drop(host);
    server.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn shutdown_closes_unstarted_connections_and_rejects_new_ones() {
    let harness = Harness::default();
    let host = attach(
        &harness,
        Session::memory(),
        SessionConfig::default(),
        model(),
    );
    let mut connection = host.connect().unwrap();
    harness.shutdown().await;
    assert!(matches!(
        connection.events.try_recv(),
        Err(broadcast::error::TryRecvError::Closed)
    ));
    assert!(host.connect().is_err());
    let late = attach(
        &harness,
        Session::memory(),
        SessionConfig::default(),
        model(),
    );
    assert!(late.connect().is_err());
    assert!(
        late.send(SessionCommand::Rename("late".into()))
            .await
            .is_err()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_during_draft_creation_keeps_the_same_run_identity() {
    let root = std::env::temp_dir().join(format!("harness-creating-stop-{}", SessionId::new()));
    let harness = Harness::default();
    let (model, server) = tool_model().await;
    let host = harness.attach(
        SessionSetup::new(model, "test", Session::memory(), "."),
        "project".into(),
        Some(root.clone()),
        SessionConfig::default(),
    );
    let mut connection = host.connect().unwrap();
    let admitted = host.try_send(submit(InputId::random())).unwrap();
    let run = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let SessionUpdate::Changed(snapshot) = connection.events.recv().await.unwrap()
                && snapshot.status == RuntimeStatus::Creating
            {
                break snapshot.run;
            }
        }
    })
    .await
    .unwrap();
    host.send(SessionCommand::Stop { run }).await.unwrap();
    let _ = admitted.await.unwrap();
    assert_eq!(host.snapshot().unwrap().status, RuntimeStatus::Idle);
    harness.shutdown().await;
    drop(host);
    server.abort();
    std::fs::remove_dir_all(root).unwrap();
}
