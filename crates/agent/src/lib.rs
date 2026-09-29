#![cfg_attr(
    not(test),
    deny(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::unwrap_used
    )
)]

pub mod context;
mod model;
pub mod session;

pub use async_openai::types::responses::{EasyInputMessage, InputItem};
pub use context::compaction::CompactionConfig;
pub use model::ReasoningEffort;
pub use session::event::*;
pub use session::machine::{PendingInput, PlannedBatch, SessionMachine, SessionMachineError};
pub use session::tree::{
    ConversationNode, ConversationNodeId, ConversationNodeKind, ConversationTree, ForkOrigin,
};
pub use session::{SessionConfig, SessionId, SessionModelConfig};

pub use session::transition::{AgentEffect, AgentInput, Transition};
pub fn validate_events(events: &[RecordedEvent]) -> Result<(), SessionMachineError> {
    SessionMachine::from_events(events).map(|_| ())
}
