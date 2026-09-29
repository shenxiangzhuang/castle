//! A session has one execution owner. Connections observe it; dropping a connection never stops it.
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_openai::types::responses::FunctionToolCall;
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::{
    CommitReceipt, InputId, InputOrigin, Model, RecordedEvent, Session, SessionConfig, SessionId,
    SessionInfo,
};

mod context;
mod execution;
use context::Agent;
use execution::{ActiveAgent, AgentEvent};
pub use execution::{AgentError, RunFailure, RunSummary};

const CAPACITY: usize = 128;

mod handle;
mod owner;
mod protocol;
pub use handle::{Harness, SessionConnection, SessionHandle};
use owner::Owner;
pub use protocol::{
    CommandResult, RuntimeSnapshot, RuntimeStatus, SessionCommand, SessionSetup, SessionUpdate,
    SubmitMode,
};

type Reply = oneshot::Sender<Result<CommandResult, String>>;
struct Envelope {
    id: crate::TxId,
    command: SessionCommand,
    reply: Reply,
}
struct Published {
    snapshot: RuntimeSnapshot,
    updates: Option<broadcast::Sender<SessionUpdate>>,
}
struct Boot {
    owner: Owner,
    commands: mpsc::Receiver<Envelope>,
    tasks: tokio_util::task::TaskTracker,
    gate: Arc<Mutex<()>>,
}

#[cfg(test)]
mod tests;
