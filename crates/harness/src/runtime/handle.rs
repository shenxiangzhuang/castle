//! Client connection, bounded command admission and application shutdown.
use super::*;

/// Application lifetime. Shutting down cancels and joins every session it started.
#[derive(Clone, Default)]
pub struct Harness {
    stopping: tokio_util::sync::CancellationToken,
    tasks: tokio_util::task::TaskTracker,
    gate: Arc<Mutex<()>>,
    connections: Arc<Mutex<Vec<std::sync::Weak<Mutex<Published>>>>>,
}
impl Harness {
    pub fn attach(
        &self,
        setup: SessionSetup,
        project: String,
        directory: Option<PathBuf>,
        config: SessionConfig,
    ) -> SessionHandle {
        let handle = SessionHandle::attach(self, setup, project, directory, config);
        let mut connections = self
            .connections
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if self.stopping.is_cancelled() {
            handle
                .published
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .updates = None;
        }
        connections.retain(|connection| connection.strong_count() > 0);
        connections.push(Arc::downgrade(&handle.published));
        handle
    }
    pub async fn shutdown(&self) {
        {
            let _gate = self.gate.lock().unwrap_or_else(|error| error.into_inner());
            self.stopping.cancel();
            self.tasks.close();
        }
        self.tasks.wait().await;
        let mut connections = self
            .connections
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for connection in connections
            .drain(..)
            .filter_map(|connection| connection.upgrade())
        {
            connection
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .updates = None;
        }
    }
}

#[derive(Clone)]
pub struct SessionHandle {
    commands: mpsc::Sender<Envelope>,
    published: Arc<Mutex<Published>>,
    boot: Arc<Mutex<Option<Boot>>>,
}

pub struct SessionConnection {
    pub snapshot: RuntimeSnapshot,
    pub events: broadcast::Receiver<SessionUpdate>,
}

impl SessionHandle {
    /// A persistent draft is materialized on its first submission. None keeps an in-memory session.
    pub fn new(
        setup: SessionSetup,
        project: String,
        directory: Option<PathBuf>,
        config: SessionConfig,
    ) -> Self {
        Harness::default().attach(setup, project, directory, config)
    }

    fn attach(
        harness: &Harness,
        setup: SessionSetup,
        project: String,
        directory: Option<PathBuf>,
        config: SessionConfig,
    ) -> Self {
        let agent = setup.agent;
        let snapshot = RuntimeSnapshot {
            session: agent.info.clone(),
            config,
            status: RuntimeStatus::Idle,
            revision: agent.revision,
            generation: 0,
            run: crate::RunId::random(),
            run_number: 0,
            completed_runs: 0,
            started_at: None,
            approvals: VecDeque::new(),
            events: setup.events,
            can_branch: agent.can_change_conversation(),
            summary: None,
        };
        let (updates, _) = broadcast::channel(CAPACITY);
        let published = Arc::new(Mutex::new(Published {
            snapshot: snapshot.clone(),
            updates: Some(updates),
        }));
        let (commands, receiver) = mpsc::channel(CAPACITY);
        let owner = Owner {
            agent: Some(agent),
            active: None,
            snapshot,
            project,
            directory,
            published: published.clone(),
            stop_replies: Vec::new(),
            admission: None,
            terminal: None,
            shutting_down: false,
            ledger: None,
            stopping: harness.stopping.clone(),
        };
        Self {
            commands,
            published,
            boot: Arc::new(Mutex::new(Some(Boot {
                owner,
                commands: receiver,
                tasks: harness.tasks.clone(),
                gate: harness.gate.clone(),
            }))),
        }
    }

    pub fn connect(&self) -> Result<SessionConnection, String> {
        let published = self
            .published
            .lock()
            .map_err(|_| "session publication lock poisoned")?;
        // The same lock covers publication, snapshot acquisition and subscription.
        Ok(SessionConnection {
            snapshot: published.snapshot.clone(),
            events: published
                .updates
                .as_ref()
                .ok_or("session owner is closed")?
                .subscribe(),
        })
    }

    pub fn snapshot(&self) -> Result<RuntimeSnapshot, String> {
        self.published
            .lock()
            .map(|state| state.snapshot.clone())
            .map_err(|_| "session publication lock poisoned".into())
    }

    fn start_owner(&self) -> Result<(), String> {
        let mut boot = self
            .boot
            .lock()
            .map_err(|_| "session startup lock poisoned")?;
        if boot.is_some() {
            let runtime =
                tokio::runtime::Handle::try_current().map_err(|error| error.to_string())?;
            if let Some(boot) = boot.take() {
                let _gate = boot
                    .gate
                    .lock()
                    .map_err(|_| "harness lifecycle lock poisoned")?;
                if boot.owner.stopping.is_cancelled() {
                    return Err("harness is shut down".into());
                }
                boot.tasks.spawn_on(boot.owner.run(boot.commands), &runtime);
            }
        }
        Ok(())
    }

    /// Queue admission only; the returned receiver resolves after the operation's semantic boundary.
    pub fn try_send(
        &self,
        command: SessionCommand,
    ) -> Result<oneshot::Receiver<Result<CommandResult, String>>, String> {
        self.try_send_with_id(crate::TxId::random(), command)
    }

    pub fn try_send_with_id(
        &self,
        id: crate::TxId,
        command: SessionCommand,
    ) -> Result<oneshot::Receiver<Result<CommandResult, String>>, String> {
        self.start_owner()?;
        let (reply, received) = oneshot::channel();
        self.commands
            .try_send(Envelope { id, command, reply })
            .map_err(|error| error.to_string())?;
        Ok(received)
    }

    pub async fn send(&self, command: SessionCommand) -> Result<CommandResult, String> {
        self.send_with_id(crate::TxId::random(), command).await
    }

    pub async fn send_with_id(
        &self,
        id: crate::TxId,
        command: SessionCommand,
    ) -> Result<CommandResult, String> {
        self.start_owner()?;
        let (reply, received) = oneshot::channel();
        self.commands
            .send(Envelope { id, command, reply })
            .await
            .map_err(|error| error.to_string())?;
        received
            .await
            .map_err(|_| "session owner ended before acknowledgement".to_owned())?
    }
}
