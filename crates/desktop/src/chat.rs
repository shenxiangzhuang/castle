//! Chat rendering, input and retained presentation working set.
mod composer;
pub(crate) mod html_preview;
mod presentation;
mod scroll;
mod view;
mod viewport;
pub(crate) use presentation::{MessagePresentation, MessagePresentationStore};
pub(crate) use scroll::ScrollAnchor;
pub(crate) use viewport::{ChatViewport, MAX_CODE_SOURCE_BYTES, MAX_INDEX_SOURCE_BYTES, RowKey};
