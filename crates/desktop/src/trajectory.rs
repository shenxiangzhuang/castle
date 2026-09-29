//! Execution timeline and details presentation over the shared session projection.
pub(crate) mod timeline;
mod view;
pub(crate) use timeline::TimelineMode;
pub(crate) use view::{
    TimelineModelCache, TrajectoryDetailsLayoutState, TrajectoryDetailsMarkdownCache,
};
