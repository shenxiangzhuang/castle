//! Pure layout arithmetic shared by renderers.
mod container;
mod markdown;
mod table;
pub(crate) use container::{ContainerInput, resolve_container};
pub(crate) use markdown::list_marker_width;
pub(crate) use table::{ColumnSpec, allocate_columns};
