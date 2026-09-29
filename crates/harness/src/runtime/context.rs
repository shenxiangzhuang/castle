use super::execution;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use async_openai::types::responses::Tool;

use crate::context::compaction::CompactionConfig;
use crate::model::Model;
use crate::runtime::execution::{ActiveAgent, AgentError};
use crate::session::event::EventTime;
use crate::session::machine::SessionMachine;
use crate::session::store::{MetadataUpdate, SessionStore, SessionWriterPermit};
use crate::session::{Session, SessionConfig, SessionInfo, SessionParts};
use crate::tools::{AgentTool, Env, ShellTool};

const DEFAULT_MAX_TURNS: usize = 100;

#[derive(Clone)]
pub(in crate::runtime) struct EventClock {
    inner: Arc<EventClockInner>,
}

struct EventClockInner {
    id: String,
    started: Instant,
}

impl EventClock {
    fn new() -> Self {
        Self {
            inner: Arc::new(EventClockInner {
                id: uuid::Uuid::new_v4().to_string(),
                started: Instant::now(),
            }),
        }
    }

    pub(in crate::runtime) fn now(&self) -> EventTime {
        EventTime {
            wall_time_ms: system_time_ms(),
            clock_id: self.inner.id.clone(),
            monotonic_ns: u64::try_from(self.inner.started.elapsed().as_nanos())
                .unwrap_or(u64::MAX),
        }
    }
}

/// Idle owner of one replayable session and the dependencies used by its next operation.
pub struct Agent {
    pub(in crate::runtime) model: Model,
    pub(in crate::runtime) instructions: String,
    pub(in crate::runtime) machine: SessionMachine,
    pub(in crate::runtime) store: SessionStore,
    pub(in crate::runtime) revision: u64,
    pub(in crate::runtime) writer: Option<SessionWriterPermit>,
    pub(in crate::runtime) info: SessionInfo,
    pub(in crate::runtime) session_config: SessionConfig,
    pub(in crate::runtime) env: Env,
    pub(in crate::runtime) tools: Vec<Arc<dyn AgentTool>>,
    pub(in crate::runtime) compaction: Option<CompactionConfig>,
    pub(in crate::runtime) max_turns: usize,
    pub(in crate::runtime) clock: EventClock,
}

impl Agent {
    #[allow(clippy::expect_used, reason = "DEFAULT_MAX_TURNS is nonzero")]
    pub fn new(
        model: Model,
        instructions: impl Into<String>,
        session: Session,
        cwd: impl Into<PathBuf>,
    ) -> Self {
        let context_window = model.context_window();
        Self::from_session_parts(
            model,
            instructions.into(),
            session.into_parts(),
            cwd.into(),
            vec![Arc::new(ShellTool)],
            Some(CompactionConfig::new(context_window)),
            DEFAULT_MAX_TURNS,
        )
        .expect("default max turns is valid")
    }

    fn from_session_parts(
        model: Model,
        instructions: String,
        parts: SessionParts,
        cwd: PathBuf,
        tools: Vec<Arc<dyn AgentTool>>,
        compaction: Option<CompactionConfig>,
        max_turns: usize,
    ) -> Result<Self, AgentError> {
        if max_turns == 0 {
            return Err(AgentError::MaxTurns(0));
        }
        Ok(Self {
            model,
            instructions,
            machine: parts.machine,
            store: parts.store,
            revision: parts.revision,
            writer: None,
            info: parts.info,
            session_config: parts.config,
            env: Env { cwd },
            tools,
            compaction,
            max_turns,
            clock: EventClock::new(),
        })
    }

    pub fn model(&self) -> &Model {
        &self.model
    }

    pub fn session_info(&self) -> &SessionInfo {
        &self.info
    }

    /// Revision of the canonical session snapshot currently owned by this agent.
    ///
    /// UI runtimes use this together with the session config to decide whether an idle,
    /// cached agent still represents the current store snapshot before reusing it.
    #[cfg(test)]
    pub fn session_revision(&self) -> u64 {
        self.revision
    }

    /// Durable configuration loaded into the session snapshot currently owned by this agent.
    pub fn set_model(&mut self, model: Model) {
        self.compaction = Some(CompactionConfig::new(model.context_window()));
        self.model = model;
    }

    pub fn set_session(&mut self, session: Session) {
        let parts = session.into_parts();
        self.machine = parts.machine;
        self.store = parts.store;
        self.revision = parts.revision;
        self.writer = None;
        self.info = parts.info;
        self.session_config = parts.config;
        self.clock = EventClock::new();
    }

    pub async fn rename_session(&mut self, title: &str) -> Result<(), AgentError> {
        let store = self.store.clone();
        let id = self.info.id.clone();
        let title = title.to_owned();
        let writer = self.acquire_or_clone_writer().await?;
        let metadata = tokio::task::spawn_blocking(move || {
            store.update_metadata(
                &id,
                MetadataUpdate {
                    title: Some(title),
                    ..MetadataUpdate::default()
                },
                &writer,
            )
        })
        .await
        .map_err(|error| AgentError::Task(error.to_string()))??;
        self.info.title = metadata.title;
        self.info.updated_at = millis_to_seconds(metadata.updated_at_ms);
        Ok(())
    }

    pub async fn persist_session_config(
        &mut self,
        config: &SessionConfig,
    ) -> Result<(), AgentError> {
        let store = self.store.clone();
        let id = self.info.id.clone();
        let new_config = config.clone();
        let writer = self.acquire_or_clone_writer().await?;
        let metadata = tokio::task::spawn_blocking(move || {
            store.update_metadata(
                &id,
                MetadataUpdate {
                    config: Some(new_config),
                    ..MetadataUpdate::default()
                },
                &writer,
            )
        })
        .await
        .map_err(|error| AgentError::Task(error.to_string()))??;
        self.session_config = config.clone();
        self.info.updated_at = millis_to_seconds(metadata.updated_at_ms);
        Ok(())
    }

    pub fn tool_schemas(&self) -> Vec<Tool> {
        self.tools.iter().map(|tool| tool.schema()).collect()
    }

    #[cfg(test)]
    pub fn start(self, input: impl Into<String>) -> ActiveAgent {
        execution::start(self, input.into())
    }

    /// Start with a caller-owned identity so admission can be matched to its durable receipt.
    pub fn start_input(self, input_id: crate::InputId, input: String) -> ActiveAgent {
        execution::start_input(self, input_id, input)
    }

    pub fn can_change_conversation(&self) -> bool {
        !self.info.is_archived() && self.machine.require_quiescent().is_ok()
    }

    pub fn edit_input(
        self,
        input_id: crate::InputId,
        input: String,
        target: crate::InputId,
        revision: u64,
        head: Option<u64>,
    ) -> ActiveAgent {
        execution::edit_input(self, input_id, input, target, revision, head)
    }

    #[cfg(test)]
    pub async fn select_conversation(
        self,
        target: Option<u64>,
        revision: u64,
        head: Option<u64>,
    ) -> (Self, Vec<crate::CommitReceipt>, Result<(), AgentError>) {
        execution::select_conversation(self, target, revision, head).await
    }

    pub async fn fork_session(
        self,
        child: crate::SessionId,
        anchor: u64,
        target: Option<u64>,
        revision: u64,
        head: Option<u64>,
    ) -> (Self, Vec<crate::CommitReceipt>, Result<Session, AgentError>) {
        execution::fork_session(self, child, anchor, target, revision, head).await
    }

    pub fn resume_pending(self) -> ActiveAgent {
        execution::resume_pending(self)
    }

    /// Mutate an idle queue, returning ownership and every committed projection update.
    pub async fn cancel_pending_input(
        self,
        input_id: crate::InputId,
    ) -> (
        Self,
        Vec<crate::session::store::CommitReceipt>,
        Result<(), AgentError>,
    ) {
        execution::cancel_pending_input(self, input_id).await
    }

    pub fn start_compaction(self, instructions: Option<String>) -> ActiveAgent {
        execution::start_compaction(self, instructions)
    }

    pub(in crate::runtime) async fn acquire_or_clone_writer(
        &self,
    ) -> Result<SessionWriterPermit, AgentError> {
        if let Some(writer) = &self.writer {
            return Ok(writer.clone());
        }
        let store = self.store.clone();
        let id = self.info.id.clone();
        tokio::task::spawn_blocking(move || store.acquire_writer(&id))
            .await
            .map_err(|error| AgentError::Task(error.to_string()))?
            .map_err(Into::into)
    }
}

fn millis_to_seconds(millis: i64) -> u64 {
    u64::try_from(millis.max(0)).unwrap_or(0) / 1_000
}

fn system_time_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}
