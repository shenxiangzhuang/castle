//! Operating-system integration.
pub(crate) mod crash;
mod native_titlebar;
pub(crate) mod updater;
pub(crate) use native_titlebar::NativeTitlebarController;

pub(crate) mod display;

/// X11 has no GPUI window-control hit testing. Its drag regions need explicit operations.
pub(crate) fn linux_titlebar_mouse_down(
    event: &gpui_kit::MouseDownEvent,
    window: &mut gpui_kit::Window,
    _cx: &mut gpui_kit::App,
) {
    if cfg!(target_os = "linux") {
        if event.click_count == 2 {
            window.zoom_window();
        } else {
            window.start_window_move();
        }
    }
}

/// Reuse the framework's OS control semantics and compositor-supported controls.
/// Keep them outside the virtualized chat so empty, archived, and preview views retain them.
pub(crate) fn window_controls(
    window: &gpui_kit::Window,
    cx: &gpui_kit::App,
) -> Option<gpui_kit::AnyElement> {
    use gpui_kit::component::TitleBar;
    use gpui_kit::{IntoElement, ParentElement, Styled, div, px};
    let width = window_controls_width(window);
    if width == 0.0 {
        return None;
    }
    Some(
        div()
            .absolute()
            .top_0()
            .right_0()
            .child(
                TitleBar::new()
                    .pl_0()
                    .w(px(width))
                    .h(px(crate::rendering::theme::metrics::TITLEBAR_HEIGHT))
                    .border_0()
                    .bg(crate::rendering::theme::palette(cx).surface),
            )
            .into_any_element(),
    )
}

/// The same reservation is used by headers, including fullscreen and server decorations.
pub(crate) fn window_controls_width(window: &gpui_kit::Window) -> f32 {
    if cfg!(target_os = "macos")
        || window.is_fullscreen()
        || (cfg!(target_os = "linux")
            && !matches!(
                window.window_decorations(),
                gpui_kit::Decorations::Client { .. }
            ))
    {
        return 0.0;
    }
    let supported = window.window_controls();
    34.0 * f32::from(1 + u8::from(supported.minimize) + u8::from(supported.maximize))
}
