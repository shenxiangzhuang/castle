//! Shared rendering primitives. No session commands or application coordination.
pub(crate) mod assets;
pub(crate) mod automation;
pub(crate) mod frame_clock;
pub(crate) mod layout;
pub(crate) mod markdown;
mod measured_container;
pub(crate) mod streaming_markdown;
pub(crate) mod syntax;
mod text_selection;
pub(crate) mod theme;
pub(crate) use measured_container::measured_container;
pub(crate) use text_selection::{MessageSelection, SelectionFragment, SelectionFrame};
