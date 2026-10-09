//! Match GPUI's Linux window to WebKitGTK's X11 native-child backend.
use std::error::Error;
#[cfg(any(test, target_os = "linux"))]
use std::ffi::OsStr;

#[cfg(any(test, target_os = "linux"))]
fn needs_x11_reexec(display: Option<&OsStr>, wayland: Option<&OsStr>, headless: bool) -> bool {
    !headless
        && display.is_some_and(|value| !value.is_empty())
        && wayland.is_some_and(|value| !value.is_empty())
}

pub(crate) fn prepare() -> Result<(), Box<dyn Error>> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        let display = std::env::var_os("DISPLAY");
        let wayland = std::env::var_os("WAYLAND_DISPLAY");
        let headless = std::env::var_os("ZED_HEADLESS").is_some();
        if needs_x11_reexec(display.as_deref(), wayland.as_deref(), headless) {
            // Replace the process before creating UI/runtime threads. Changing the environment
            // in place is unsafe in a multithreaded process; preserve every argument and the
            // rest of the environment (including XDG_SESSION_TYPE and portal configuration).
            return Err(std::process::Command::new(std::env::current_exe()?)
                .args(std::env::args_os().skip(1))
                .env_remove("WAYLAND_DISPLAY")
                .exec()
                .into());
        }
        if !headless && display.as_deref().is_none_or(OsStr::is_empty) {
            return Err("Castle requires X11 or XWayland for inline HTML previews. Enable XWayland in the desktop session and launch Castle again.".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn linux_display_selection_preserves_x11_and_headless_and_uses_xwayland() {
        let x11 = Some(OsStr::new(":0"));
        let wayland = Some(OsStr::new("wayland-0"));
        assert!(needs_x11_reexec(x11, wayland, false));
        assert!(!needs_x11_reexec(x11, None, false));
        assert!(!needs_x11_reexec(None, wayland, false));
        assert!(!needs_x11_reexec(Some(OsStr::new("")), wayland, false));
        assert!(!needs_x11_reexec(x11, Some(OsStr::new("")), false));
        assert!(!needs_x11_reexec(x11, wayland, true));
    }
}
