//! Operating-system integration.
pub(crate) mod crash;
mod native_titlebar;
pub(crate) mod updater;
pub(crate) use native_titlebar::NativeTitlebarController;
