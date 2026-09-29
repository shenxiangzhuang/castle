//! Commands, committed updates and snapshot data shared with clients.
use super::*;

/// Construction data, never a running agent. Consumed by the session owner.
pub struct SessionSetup {
    pub(super) agent: Agent,
    pub(super) events: im::Vector<RecordedEvent>,
}

impl SessionSetup {
    pub fn new(
        model: Model,
        instructions: impl Into<String>,
        session: Session,
        cwd: impl Into<PathBuf>,
    ) -> Self {
        let events = session.events().iter().cloned().collect();
        Self {
            agent: Agent::new(model, instructions, session, cwd),
            events,
        }
    }
    pub fn session_info(&self) -> &SessionInfo {
        self.agent.session_info()
    }
    pub fn model(&self) -> &Model {
        self.agent.model()
    }
    pub fn set_model(&mut self, model: Model) {
        self.agent.set_model(model);
    }
    pub fn tool_schemas(&self) -> Vec<async_openai::types::responses::Tool> {
        self.agent.tool_schemas()
    }
    pub fn with_tools(mut self, tools: Vec<Arc<dyn crate::AgentTool>>) -> Self {
        self.agent.tools = tools;
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeStatus {
    Idle,
    Creating,
    Configuring,
    Running,
    Settling,
    Failed(RunFailure),
}
impl RuntimeStatus {
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            Self::Creating | Self::Configuring | Self::Running | Self::Settling
        )
    }
}

#[derive(Clone)]
pub struct RuntimeSnapshot {
    pub session: SessionInfo,
    pub config: SessionConfig,
    pub status: RuntimeStatus,
    pub revision: u64,
    pub generation: u64,
    pub run: crate::RunId,
    pub run_number: u64,
    pub completed_runs: u64,
    pub started_at: Option<Instant>,
    pub approvals: VecDeque<FunctionToolCall>,
    pub events: im::Vector<RecordedEvent>,
    pub can_branch: bool,
    pub summary: Option<crate::RunSummary>,
}

#[derive(Clone, Copy, Debug, serde::Serialize)]
pub enum SubmitMode {
    Start,
    Queue,
    Steer,
}

#[derive(serde::Serialize)]
pub enum SessionCommand {
    Submit {
        id: InputId,
        text: String,
        mode: SubmitMode,
    },
    Edit {
        id: InputId,
        text: String,
        target: InputId,
        revision: u64,
        head: Option<u64>,
    },
    ResumePending,
    Compact {
        instructions: Option<String>,
    },
    CancelInput(InputId),
    Prioritize(InputId),
    Decide {
        run: crate::RunId,
        call_id: String,
        allow: bool,
    },
    Configure {
        config: SessionConfig,
        model: Option<Model>,
    },
    RefreshModel {
        expected_model: Option<String>,
        model: Model,
    },
    Rename(String),
    Fork {
        child: SessionId,
        anchor: u64,
        target: Option<u64>,
        revision: u64,
        head: Option<u64>,
    },
    Stop {
        run: crate::RunId,
    },
    Shutdown,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum CommandResult {
    Applied,
    Forked(SessionInfo),
}

#[derive(Clone)]
pub enum SessionUpdate {
    Committed(CommitReceipt),
    Changed(Box<RuntimeSnapshot>),
    CommandResult {
        id: crate::TxId,
        result: Result<CommandResult, String>,
    },
}
