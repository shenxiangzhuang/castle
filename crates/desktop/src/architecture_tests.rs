use std::fs;
use std::path::{Path, PathBuf};

#[test]
fn workspace_dependencies_enforce_sdk_harness_desktop_layers() {
    let desktop = Path::new(env!("CARGO_MANIFEST_DIR"));
    let sdk = fs::read_to_string(desktop.join("../agent/Cargo.toml")).unwrap();
    for dependency in ["tokio", "rusqlite", "libc", "gpui-kit", "harness"] {
        assert!(
            !sdk.lines().any(|line| line.starts_with(dependency)),
            "SDK depends on {dependency}"
        );
    }
    let frontend = fs::read_to_string(desktop.join("Cargo.toml")).unwrap();
    assert!(frontend.contains("harness.workspace = true"));
    assert!(!frontend.contains("agent.workspace = true"));
}

fn rust_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_owned()];
    while let Some(path) = pending.pop() {
        for entry in fs::read_dir(path).expect("source directory should be readable") {
            let path = entry.expect("source entry should be readable").path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                files.push(path);
            }
        }
    }
    files
}

#[test]
fn pure_projections_and_layout_do_not_depend_on_gpui_or_execution() {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut paths = rust_files(&source.join("rendering/layout"));
    paths.extend(
        [
            "app/action.rs",
            "app/reducer.rs",
            "app/state.rs",
            "app/layout.rs",
            "app/presentation.rs",
            "session/document.rs",
            "session/ids.rs",
            "session/message.rs",
            "session/trajectory.rs",
            "session/view.rs",
            "trajectory/timeline.rs",
            "chat/scroll.rs",
            "rendering/streaming_markdown.rs",
        ]
        .map(|path| source.join(path)),
    );
    for path in paths {
        let text = fs::read_to_string(&path).expect("source file should be readable");
        for forbidden in [
            "gpui_kit::",
            "use gpui",
            "std::fs::",
            "tokio::",
            "SessionCommand",
            "SessionHandle",
        ] {
            assert!(
                !text.contains(forbidden),
                "{forbidden} entered pure projection/layout: {}",
                path.display()
            );
        }
    }
}

#[test]
fn draw_phase_apis_are_confined_to_rendering_adapters() {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let adapters = [
        source.join("rendering/frame_clock.rs"),
        source.join("rendering/measured_container.rs"),
        source.join("chat/viewport.rs"),
    ];
    let guard = source.join("architecture_tests.rs");
    for path in rust_files(&source) {
        if adapters.contains(&path) || path == guard {
            continue;
        }
        let text = fs::read_to_string(&path).expect("source file should be readable");
        for forbidden in [".on_next_frame(", ".layout_bounds("] {
            assert!(
                !text.contains(forbidden),
                "{forbidden} escaped the GPUI adapter: {}",
                path.display()
            );
        }
    }
}

#[test]
fn desktop_app_has_no_state_deref_escape_hatch() {
    let app = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("app.rs"),
    )
    .expect("app source should be readable");
    assert!(!app.contains("impl Deref for DesktopApp"));
    assert!(!app.contains("impl DerefMut for DesktopApp"));
}

#[test]
fn sidebar_rendering_does_not_list_sessions_from_disk() {
    let sidebar = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("workspace/sidebar.rs"),
    )
    .expect("sidebar source should be readable");
    assert!(
        !sidebar.contains("Session::list") && !sidebar.contains("std::fs::"),
        "sidebar rendering must consume cached session metadata"
    );
}

#[test]
fn harness_public_api_excludes_execution_internals_and_presentation_policy() {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("harness")
        .join("src")
        .join("lib.rs");
    let public_api = fs::read_to_string(source).expect("agent public API should be readable");
    for forbidden in [
        "TranscriptItem",
        "SessionSearchData",
        "SessionMachine,",
        "PlannedBatch",
        "ModelPreset",
        "PROVIDER_ID",
        "ActiveAgent",
        "AgentEvent",
        "SessionWriterPermit",
        "pub mod runtime",
        "pub use runtime::*",
    ] {
        assert!(
            !public_api.contains(forbidden),
            "desktop presentation policy leaked into agent public API: {forbidden}"
        );
    }
}

#[test]
fn shared_rendering_does_not_depend_on_features_or_session_execution() {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut paths = rust_files(&source.join("rendering"));
    paths.push(source.join("rendering.rs"));
    for path in paths {
        let text = fs::read_to_string(&path).unwrap();
        for forbidden in [
            "DesktopApp",
            "crate::app",
            "crate::chat",
            "crate::trajectory",
            "crate::workspace",
            "SessionCommand",
            "SessionHandle",
        ] {
            assert!(
                !text.contains(forbidden),
                "{forbidden} entered shared rendering: {}",
                path.display()
            );
        }
    }
}
