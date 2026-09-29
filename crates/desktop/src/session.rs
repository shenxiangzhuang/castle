//! Canonical committed-event projection and its harness connection.
mod connection;
pub(crate) mod document;
mod ids;
pub(crate) mod message;
pub(crate) mod trajectory;
mod view;

pub(crate) use connection::{
    ApplicationHarness, SessionConnection, SessionConnectionSnapshot, SessionConnectionStatus,
};
pub(crate) use document::{
    ItemStatus, ModelRequestOptions, PromptChangeKind, PromptSnapshot, TrajectoryItemId,
    TrajectoryRequestKey, TrajectoryRequestPurpose,
};
pub(crate) use ids::{LayoutGeneration, MessageId, RunId, next_message_id};
pub(crate) use message::{Message, Role};
#[cfg(test)]
pub(crate) use trajectory::RecordTiming;
pub(crate) use trajectory::{
    TrajectoryChanges, TrajectoryKind, TrajectoryProjection, TrajectoryRecord,
    TrajectoryRecordDetails, TrajectoryRequest,
};
pub(crate) use view::SessionView;
