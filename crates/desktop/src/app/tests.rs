use super::*;

fn close_test_window(view: Entity<DesktopApp>, cx: &mut gpui_kit::VisualTestContext) {
    let harness = cx.update(|_, cx| cx.global::<crate::session::ApplicationHarness>().0.clone());
    let weak_view = view.downgrade();
    drop(view);
    cx.update(|window, _| window.remove_window());
    cx.run_until_parked();
    // Execution now outlives the UI subscription. Join before deleting SQLite files,
    // including on Windows where an open connection prevents directory removal.
    if let Ok(executor) = tokio::runtime::Handle::try_current() {
        executor.block_on(harness.shutdown());
    }
    assert!(
        weak_view.upgrade().is_none(),
        "closing the test window must release its app and session database handles"
    );
}

#[gpui_kit::test]
fn fork_completion_keeps_project_identity_after_removal(cx: &mut gpui_kit::TestAppContext) {
    let executor = tokio::runtime::Runtime::new().unwrap();
    let _entered = executor.enter();
    cx.background_executor.allow_parking();
    let root = std::env::temp_dir().join(format!("castle-fork-project-{}", SessionId::new()));
    let (mut project_store, _) = ProjectStore::load(root.join("state"), None).unwrap();
    let mut indices = Vec::new();
    for name in ["before", "source", "after"] {
        let path = root.join(name);
        std::fs::create_dir_all(&path).unwrap();
        indices.push(project_store.add(path).unwrap());
    }
    let project = project_store.project(indices[1]).unwrap().clone();
    let after = project_store.project(indices[2]).unwrap().clone();
    let info = create_v2_session(
        &project.sessions_dir,
        project.id.as_str(),
        SessionId::new(),
        Some("Source"),
    );
    commit_external_turn(
        &info.path,
        project.id.as_str(),
        "Question",
        "Completed reply",
    );
    let model = Model::new("test", "key", "http://127.0.0.1:1", "test-model", 10_000);
    let agent = SessionSetup::new(
        model.clone(),
        "test",
        Session::memory(),
        project.path.clone(),
    );
    let configured = ConfiguredModel::new(
        "test",
        ProviderModel::new("test-model", "Test", 10_000, None),
        model,
    );
    let settings = SettingsStore::load(root.join("settings")).unwrap();
    cx.update(crate::bootstrap::init_ui);
    let (view, cx) = cx.add_window_view(|window, cx| {
        DesktopApp::new(
            DesktopStartup {
                agent,
                models: vec![configured],
                selected_model: 0,
                project_store,
                active_project: indices[1],
                settings,
            },
            window,
            cx,
        )
    });
    cx.update(|window, cx| {
        view.update(cx, |app, cx| {
            let session =
                Session::open_writable_in_project(&info.path, project.id.as_str()).unwrap();
            let runtime = app.create_runtime(indices[1], session, cx).unwrap();
            app.select_runtime(runtime, cx);
            let key = app
                .core
                .session_view
                .conversation
                .messages
                .iter()
                .find(|message| message.role == Role::Assistant)
                .unwrap()
                .key;
            assert!(app.can_fork_message(key, cx));
            app.fork_message(key, window, cx);
            // Mutation happens before the fork future can publish its result on the UI thread.
            app.remove_project(indices[0], window, cx);
            assert_eq!(app.project_store.project(indices[1]).unwrap().id, after.id);
        })
    });
    let deadline = Instant::now() + std::time::Duration::from_secs(15);
    loop {
        cx.run_until_parked();
        if view.read_with(cx, |app, cx| {
            app.project_runtimes.values().any(|project| {
                project
                    .sessions
                    .values()
                    .any(|runtime| runtime.read(cx).tree().origin.is_some())
            })
        }) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "fork completion was not published"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    view.read_with(cx, |app, cx| {
        let child = app.project_runtimes[&project.id]
            .sessions
            .values()
            .find(|runtime| runtime.read(cx).tree().origin.is_some())
            .expect("fork must stay in its source project");
        assert_eq!(
            child.read(cx).snapshot().session.project_id,
            project.id.as_str()
        );
        assert!(app.project_runtimes.get(&after.id).is_none_or(|project| {
            project
                .sessions
                .values()
                .all(|runtime| runtime.read(cx).tree().origin.is_none())
        }));
        assert_eq!(
            app.selected_runtime.read(cx).snapshot().session.id,
            info.id,
            "stale completion must not steal selection"
        );
    });
    close_test_window(view, cx);
    std::fs::remove_dir_all(root).unwrap();
}

#[gpui_kit::test]
fn conversation_edit_fork_relations_and_drafts_render(cx: &mut gpui_kit::TestAppContext) {
    let executor = tokio::runtime::Runtime::new().unwrap();
    let _entered = executor.enter();
    cx.background_executor.allow_parking();
    let root = std::env::temp_dir().join(format!("castle-tree-ui-{}", SessionId::new()));
    let (mut startup, _) = crate::bootstrap::desktop_startup(root.clone()).unwrap();
    // A deterministic connection refusal settles each admitted message without external IO.
    let model = Model::new("test", "key", "http://127.0.0.1:1", "test-model", 10_000);
    startup.models = vec![ConfiguredModel::new(
        "test",
        ProviderModel::new("test-model", "Test", 10_000, None),
        model.clone(),
    )];
    startup.selected_model = 0;
    startup.agent.set_model(model);
    cx.update(crate::bootstrap::init_ui);
    let (view, cx) = cx.add_window_view(|window, cx| DesktopApp::new(startup, window, cx));
    cx.simulate_resize(gpui_kit::size(px(1180.0), px(720.0)));
    let settle = |view: &Entity<DesktopApp>, cx: &mut gpui_kit::VisualTestContext| {
        let deadline = Instant::now() + std::time::Duration::from_secs(15);
        loop {
            cx.run_until_parked();
            if !view.read_with(cx, |app, cx| {
                app.composer_submitting || app.selected_runtime.read(cx).is_active()
            }) {
                break;
            }
            assert!(Instant::now() < deadline, "runtime did not settle");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    };
    cx.update(|window, cx| {
        view.update(cx, |app, cx| {
            app.input.update(cx, |input, cx| {
                input.set_value("original message", window, cx)
            });
            app.submit(window, cx);
        })
    });
    settle(&view, cx);
    assert!(cx.debug_bounds("conversation-branches").is_none());
    let (source, key, _old_head) = view.read_with(cx, |app, cx| {
        let key = app
            .core
            .session_view
            .conversation
            .messages
            .iter()
            .find(|m| m.role == Role::User)
            .unwrap()
            .key;
        (
            app.selected_runtime.clone(),
            key,
            app.selected_runtime.read(cx).tree().head(),
        )
    });
    let (reply_model, reply_server) = text_stream_model("Completed reply");
    cx.update(|_window, cx| {
        view.update(cx, |app, cx| {
            assert!(
                !app.can_fork_message(key, cx),
                "user messages only expose editing"
            );
            app.models[0].model = reply_model;
            app.selected_runtime
                .update(cx, |runtime, cx| runtime.refresh_model(&app.models[0], cx));
        })
    });
    settle(&view, cx);
    cx.update(|window, cx| {
        view.update(cx, |app, cx| {
            app.input.update(cx, |input, cx| {
                input.set_value("ordinary draft", window, cx)
            });
            app.edit_message(key, window, cx);
            assert_eq!(app.input.read(cx).value().as_str(), "original message");
            app.cancel_edit(window, cx);
            assert_eq!(app.input.read(cx).value().as_str(), "ordinary draft");
            app.edit_message(key, window, cx);
            app.input.update(cx, |input, cx| {
                input.set_value("edited message", window, cx)
            });
            app.submit(window, cx);
        })
    });
    settle(&view, cx);
    reply_server.join().unwrap();
    assert!(cx.debug_bounds("edit-user-message").is_some());
    assert!(cx.debug_bounds("fork-assistant-message").is_some());
    assert!(
        cx.debug_bounds("conversation-branches").is_none(),
        "local edits must not show tree navigation"
    );
    let new_head = view.read_with(cx, |app, cx| {
        assert!(app.edit_draft.is_none());
        assert_eq!(app.input.read(cx).value().as_str(), "ordinary draft");
        assert!(app.selected_runtime.read(cx).tree().has_branches());
        app.selected_runtime.read(cx).tree().head()
    });
    assert!(cx.debug_bounds("session-relations").is_none());
    assert!(cx.debug_bounds("conversation-tree-panel").is_none());
    let key = view.read_with(cx, |app, _| {
        app.core
            .session_view
            .conversation
            .messages
            .iter()
            .find(|m| m.role == Role::Assistant)
            .unwrap()
            .key
    });
    cx.update(|window, cx| view.update(cx, |app, cx| app.fork_message(key, window, cx)));
    let deadline = Instant::now() + std::time::Duration::from_secs(15);
    while view.read_with(cx, |app, _| app.selected_runtime == source) {
        assert!(Instant::now() < deadline);
        cx.run_until_parked();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    settle(&view, cx);
    assert!(cx.debug_bounds("fork-origin").is_none());
    assert!(
        cx.debug_bounds("session-relations").is_some(),
        "forked child shows its relationship icon"
    );
    let (child_path, source_id, child_id) = view.read_with(cx, |app, cx| {
        assert_eq!(app.input.read(cx).value().as_str(), "");
        assert_eq!(
            app.core
                .session_view
                .conversation
                .messages
                .back()
                .unwrap()
                .text,
            "Completed reply"
        );
        assert!(app.selected_runtime.read(cx).tree().origin.is_some());
        assert_eq!(app.core.session_view.actual_stats.input_tokens(), 0);
        assert_eq!(
            app.selected_runtime.read(cx).snapshot().session.title,
            format!("{} (1)", source.read(cx).snapshot().session.title)
        );
        (
            app.core.session.current.clone(),
            source.read(cx).snapshot().session.id.to_string(),
            app.selected_runtime
                .read(cx)
                .snapshot()
                .session
                .id
                .to_string(),
        )
    });
    assert!(
        cx.debug_bounds("continued-from-chat").is_some(),
        "fork must expose its continuation link"
    );
    // Hover opens a clickable list; moving into it must not dismiss it.
    let icon = cx.debug_bounds("session-relations").unwrap();
    assert!(
        icon.size.width <= px(24.0),
        "relationship icon must stay compact"
    );
    cx.simulate_mouse_move(icon.center(), None, Default::default());
    cx.run_until_parked();
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(650));
    cx.run_until_parked();
    let parent = cx
        .debug_bounds(format!("relation-Parents-{source_id}").leak())
        .unwrap();
    let list = cx.debug_bounds("session-relations-list").unwrap();
    assert!(
        list.size.width <= px(200.0) && list.size.height <= px(64.0),
        "one link needs a compact menu: {list:?}"
    );
    let title = cx
        .debug_bounds(format!("relation-title-Parents-{source_id}").leak())
        .unwrap();
    assert!(
        (title.left() - parent.left() - px(8.0)).abs() <= px(1.0),
        "session title must align with the row's leading padding"
    );
    assert!(parent.size.height <= px(28.0));
    let age = cx
        .debug_bounds(format!("relation-age-Parents-{source_id}").leak())
        .unwrap();
    assert!(
        title.right() < age.left(),
        "time must not overlap the title"
    );
    assert!((parent.right() - age.right() - px(8.0)).abs() <= px(1.0));

    assert!(
        cx.update(|window, _| window
            .painted_quads()
            .iter()
            .any(|quad| quad.bounds == list.scale(window.scale_factor()))),
        "compact popup surface must be painted"
    );
    cx.simulate_mouse_move(parent.center(), None, Default::default());
    cx.run_until_parked();
    let continuation = cx.debug_bounds("continued-from-chat").unwrap();
    cx.simulate_click(continuation.center(), Default::default());
    let deadline = Instant::now() + std::time::Duration::from_secs(15);
    while view.read_with(cx, |app, _| {
        app.core.session.current == child_path || app.selection_pending()
    }) {
        assert!(Instant::now() < deadline);
        cx.run_until_parked();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    settle(&view, cx);
    view.read_with(cx, |app, cx| {
        assert_eq!(app.input.read(cx).value().as_str(), "ordinary draft");
        assert_eq!(app.selected_runtime.read(cx).tree().head(), new_head);
        assert!(
            app.core
                .session_view
                .conversation
                .messages
                .iter()
                .any(|m| m.text == "edited message")
        );
    });
    // Click also opens the same list, providing a keyboard-accessible alternative.
    let icon = cx.debug_bounds("session-relations").unwrap();
    cx.simulate_click(icon.center(), Default::default());
    cx.run_until_parked();
    assert!(
        cx.debug_bounds(format!("relation-Children-{child_id}").leak())
            .is_some()
    );
    assert!(
        cx.debug_bounds(format!("relation-age-Children-{child_id}").leak())
            .is_some()
    );
    let clicked_list = cx.debug_bounds("session-relations-list").unwrap();
    assert_eq!(
        clicked_list.size, list.size,
        "hover and click must use identical compact surfaces"
    );
    assert!(cx.debug_bounds("conversation-tree-panel").is_none());
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    let source_path = view.read_with(cx, |app, _| app.core.session.current.clone());
    cx.update(|window, cx| {
        view.update(cx, |app, cx| {
            app.archive_target_session(
                app.core.workspace.active_project,
                child_path.clone(),
                window,
                cx,
            );
        })
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("session-relations").is_some(),
        "archived children must retain their relationship icon"
    );
    let icon = cx.debug_bounds("session-relations").unwrap();
    cx.simulate_click(icon.center(), Default::default());
    cx.run_until_parked();
    let row = cx
        .debug_bounds(format!("relation-Children-{child_id}").leak())
        .unwrap();
    assert!(
        cx.debug_bounds(format!("relation-age-Children-{child_id}").leak())
            .is_some()
    );
    cx.simulate_click(row.center(), Default::default());
    let deadline = Instant::now() + std::time::Duration::from_secs(15);
    while view.read_with(cx, |app, cx| {
        app.selection_pending()
            || !app
                .selected_runtime
                .read(cx)
                .snapshot()
                .session
                .is_archived()
    }) {
        assert!(Instant::now() < deadline);
        cx.run_until_parked();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    cx.run_until_parked();
    assert!(cx.debug_bounds("archived-session").is_some());
    assert!(cx.debug_bounds("continued-from-chat").is_none());
    view.read_with(cx, |app, cx| {
        assert!(!app.selected_runtime.read(cx).can_branch());
        assert!(
            app.project_sessions
                .values()
                .flatten()
                .all(|s| s.id.to_string() != child_id)
        );
        assert!(
            app.project_archived_sessions
                .values()
                .flatten()
                .any(|s| s.id.to_string() == child_id)
        );
    });
    let restore = cx.debug_bounds("unarchive-and-open").unwrap();
    assert!(f32::from(restore.size.width) < 220.0);
    assert_eq!(restore.size.height, px(32.0));
    assert!(
        cx.update(|window, cx| {
            let background: gpui_kit::Background = crate::rendering::theme::palette(cx).text.into();
            window.painted_quads().iter().any(|quad| {
                quad.bounds == restore.scale(window.scale_factor()) && quad.background == background
            })
        }),
        "restore action must have an opaque contrasting background"
    );
    cx.simulate_click(restore.center(), Default::default());
    let deadline = Instant::now() + std::time::Duration::from_secs(15);
    while view.read_with(cx, |app, _| {
        app.core.session.current != child_path || app.selection_pending()
    }) {
        assert!(Instant::now() < deadline);
        cx.run_until_parked();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    settle(&view, cx);
    assert!(cx.debug_bounds("archived-session").is_none());
    assert!(cx.debug_bounds("continued-from-chat").is_some());
    cx.update(|window, cx| {
        view.update(cx, |app, cx| {
            app.archive_target_session(app.core.workspace.active_project, source_path, window, cx);
        })
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("session-relations").is_some(),
        "archived parents must retain their relationship icon"
    );
    assert!(cx.debug_bounds("continued-from-chat").is_some());
    let continuation = cx.debug_bounds("continued-from-chat").unwrap();
    cx.simulate_click(continuation.center(), Default::default());
    let deadline = Instant::now() + std::time::Duration::from_secs(15);
    while view.read_with(cx, |app, cx| {
        app.selection_pending()
            || !app
                .selected_runtime
                .read(cx)
                .snapshot()
                .session
                .is_archived()
    }) {
        assert!(Instant::now() < deadline);
        cx.run_until_parked();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    cx.run_until_parked();
    assert!(cx.debug_bounds("archived-session").is_some());
    view.read_with(cx, |app, cx| {
        assert_eq!(
            app.selected_runtime
                .read(cx)
                .snapshot()
                .session
                .id
                .to_string(),
            source_id
        );
    });
    drop(source);
    close_test_window(view, cx);
    std::fs::remove_dir_all(root).unwrap();
}

#[gpui_kit::test]
fn pending_edit_withdraws_before_resubmission_and_preserves_drafts(
    cx: &mut gpui_kit::TestAppContext,
) {
    for switch_session in [false, true] {
        let executor = tokio::runtime::Runtime::new().unwrap();
        let _entered = executor.enter();
        cx.background_executor.allow_parking();
        let root = std::env::temp_dir().join(format!("castle-pending-edit-{}", SessionId::new()));
        let (mut startup, _) = crate::bootstrap::desktop_startup(root.clone()).unwrap();
        // Leave the provider response blocked while queue commands are processed.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let model = Model::new(
            "test",
            "key",
            format!("http://{}", listener.local_addr().unwrap()),
            "test-model",
            10_000,
        );
        startup.models = vec![ConfiguredModel::new(
            "test",
            ProviderModel::new("test-model", "Test", 10_000, None),
            model.clone(),
        )];
        startup.selected_model = 0;
        startup.agent.set_model(model.clone());
        cx.update(crate::bootstrap::init_ui);
        let (view, cx) = cx.add_window_view(|window, cx| DesktopApp::new(startup, window, cx));
        cx.simulate_resize(gpui_kit::size(px(1180.0), px(720.0)));
        cx.update(|window, cx| {
            view.update(cx, |app, cx| {
                app.input
                    .update(cx, |input, cx| input.set_value("working", window, cx));
                app.submit(window, cx);
            })
        });
        let deadline = Instant::now() + std::time::Duration::from_secs(10);
        while view.read_with(cx, |app, _| app.composer_submitting) {
            assert!(Instant::now() < deadline);
            cx.run_until_parked();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        cx.update(|window, cx| {
            view.update(cx, |app, cx| {
                for text in [
                    "edit while running",
                    "edit while paused",
                    "already withdrawn",
                ] {
                    assert!(
                        app.selected_runtime
                            .update(cx, |runtime, cx| runtime.submit(text.into(), window, cx))
                            .is_some()
                    );
                }
            })
        });
        while view.read_with(cx, |app, cx| {
            app.selected_runtime.read(cx).pending_inputs().len() != 3
        }) {
            assert!(Instant::now() < deadline);
            cx.run_until_parked();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let pending = view.read_with(cx, |app, cx| app.selected_runtime.read(cx).pending_inputs());
        cx.update(|window, cx| {
            view.update(cx, |app, cx| {
                app.input.update(cx, |input, cx| {
                    input.set_value("existing draft", window, cx)
                });
                app.edit_pending(pending[0].input_id.clone(), window, cx);
                assert_eq!(app.input.read(cx).value().as_str(), "existing draft");
                assert!(!app.composer_submitting);
                assert_eq!(app.selected_runtime.read(cx).pending_inputs().len(), 3);
                app.input
                    .update(cx, |input, cx| input.set_value("", window, cx));
                app.edit_pending(pending[0].input_id.clone(), window, cx);
                assert_eq!(app.input.read(cx).value().as_str(), "edit while running");
                assert!(app.composer_submitting);
                app.input
                    .update(cx, |input, cx| input.set_value("revised draft", window, cx));
                app.submit(window, cx); // Must not submit while withdrawal is unacknowledged.
            })
        });
        while view.read_with(cx, |app, cx| {
            app.composer_submitting || app.selected_runtime.read(cx).pending_inputs().len() != 2
        }) {
            assert!(Instant::now() < deadline);
            cx.run_until_parked();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        view.read_with(cx, |app, cx| {
            assert_eq!(app.input.read(cx).value().as_str(), "revised draft")
        });
        cx.update(|_, cx| view.update(cx, |app, cx| app.abort(cx)));
        while view.read_with(cx, |app, cx| app.selected_runtime.read(cx).is_active()) {
            assert!(Instant::now() < deadline);
            cx.run_until_parked();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        cx.update(|window, cx| {
            view.update(cx, |app, cx| {
                app.input
                    .update(cx, |input, cx| input.set_value("", window, cx));
                app.edit_pending(pending[1].input_id.clone(), window, cx);
            })
        });
        while view.read_with(cx, |app, _| app.composer_submitting) {
            assert!(Instant::now() < deadline);
            cx.run_until_parked();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let path = view.read_with(cx, |app, cx| {
            assert_eq!(app.input.read(cx).value().as_str(), "edit while paused");
            assert_eq!(app.selected_runtime.read(cx).pending_inputs().len(), 1);
            app.selected_runtime.read(cx).snapshot().session.path
        });
        // Another writer withdraws the last message; the UI still has a stale row.
        executor.block_on(async {
            let session = Session::open(&path).await.unwrap();
            let agent = SessionSetup::new(model, "test", session, ".");
            let host =
                harness::SessionHandle::new(agent, String::new(), None, SessionConfig::default());
            host.send(harness::SessionCommand::CancelInput(
                pending[2].input_id.clone(),
            ))
            .await
            .unwrap();
        });
        let source = view.read_with(cx, |app, _| app.selected_runtime.clone());
        cx.update(|window, cx| {
            view.update(cx, |app, cx| {
                app.input
                    .update(cx, |input, cx| input.set_value("", window, cx));
                app.edit_pending(pending[2].input_id.clone(), window, cx);
                assert_eq!(app.input.read(cx).value().as_str(), "already withdrawn");
                if switch_session {
                    let other = app
                        .create_runtime(app.core.workspace.active_project, Session::memory(), cx)
                        .unwrap();
                    app.select_runtime(other, cx);
                    app.input
                        .update(cx, |input, cx| input.set_value("other draft", window, cx));
                }
            })
        });
        while view.read_with(cx, |app, _| app.composer_submitting) {
            assert!(Instant::now() < deadline);
            cx.run_until_parked();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        view.read_with(cx, |app, cx| {
            if switch_session {
                assert_eq!(app.input.read(cx).value().as_str(), "other draft");
                assert!(
                    app.core.transient_messages.is_empty(),
                    "a different session must not receive the edit error"
                );
            } else {
                assert!(
                    app.input.read(cx).value().is_empty(),
                    "failed withdrawal must remove the untouched copy"
                );
            }
            assert!(source.read(cx).pending_inputs().is_empty());
            assert!(source.read(cx).input_error.is_some());
        });
        drop(source);
        close_test_window(view, cx);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[gpui_kit::test]
fn admission_receipt_does_not_scroll_another_session(cx: &mut gpui_kit::TestAppContext) {
    let executor = tokio::runtime::Runtime::new().unwrap();
    let _entered = executor.enter();
    cx.background_executor.allow_parking();
    let root = std::env::temp_dir().join(format!("castle-draft-receipt-{}", SessionId::new()));
    let (mut startup, _) = crate::bootstrap::desktop_startup(root.clone()).unwrap();
    let (model, server) = text_stream_model("done");
    startup.models = vec![ConfiguredModel::new(
        "test",
        harness::config::ProviderModel::new("test-model", "Test", 10_000, None),
        model.clone(),
    )];
    startup.selected_model = 0;
    startup.agent.set_model(model);
    cx.update(crate::bootstrap::init_ui);
    let (view, cx) = cx.add_window_view(|window, cx| DesktopApp::new(startup, window, cx));
    let source = view.read_with(cx, |app, _| app.selected_runtime.clone());
    cx.update(|window, cx| {
        view.update(cx, |app, cx| {
            app.input.update(cx, |input, cx| {
                input.set_value("accepted message", window, cx)
            });
            app.submit(window, cx);
            assert!(app.composer_submitting);
            let other = app
                .create_runtime(app.core.workspace.active_project, Session::memory(), cx)
                .unwrap();
            app.select_runtime(other, cx);
            app.dispatch(Action::Scroll(ScrollIntent::Away), window, cx);
            assert!(!app.core.follow_chat_tail);
            app.input
                .update(cx, |input, cx| input.set_value("new draft", window, cx));
        })
    });
    let deadline = Instant::now() + std::time::Duration::from_secs(10);
    // The background source still owns its database until its run has joined.
    while view.read_with(cx, |app, cx| {
        app.composer_submitting || source.read(cx).is_active()
    }) {
        assert!(Instant::now() < deadline, "submission did not settle");
        cx.run_until_parked();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    server.join().unwrap();
    view.read_with(cx, |app, cx| {
        assert_eq!(app.input.read(cx).value().as_str(), "new draft");
        assert!(
            !app.core.follow_chat_tail,
            "late receipt from another session changed the current scroll policy"
        );
    });
    drop(source);
    close_test_window(view, cx);
    std::fs::remove_dir_all(root).unwrap();
}

#[gpui_kit::test]
fn admission_receipt_preserves_a_newer_draft(cx: &mut gpui_kit::TestAppContext) {
    let executor = tokio::runtime::Runtime::new().unwrap();
    let _entered = executor.enter();
    cx.background_executor.allow_parking();
    let root = std::env::temp_dir().join(format!("castle-draft-receipt-{}", SessionId::new()));
    let (mut startup, _) = crate::bootstrap::desktop_startup(root.clone()).unwrap();
    let (model, server) = text_stream_model("done");
    startup.models = vec![ConfiguredModel::new(
        "test",
        harness::config::ProviderModel::new("test-model", "Test", 10_000, None),
        model.clone(),
    )];
    startup.selected_model = 0;
    startup.agent.set_model(model);
    cx.update(crate::bootstrap::init_ui);
    let (view, cx) = cx.add_window_view(|window, cx| DesktopApp::new(startup, window, cx));
    cx.update(|window, cx| {
        view.update(cx, |app, cx| {
            app.input.update(cx, |input, cx| {
                input.set_value("accepted message", window, cx)
            });
            app.submit(window, cx);
            assert!(app.composer_submitting);
            app.input
                .update(cx, |input, cx| input.set_value("new draft", window, cx));
        })
    });
    let deadline = Instant::now() + std::time::Duration::from_secs(10);
    while view.read_with(cx, |app, cx| {
        app.composer_submitting || app.selected_runtime.read(cx).is_active()
    }) {
        assert!(Instant::now() < deadline, "submission did not settle");
        cx.run_until_parked();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    server.join().unwrap();
    view.read_with(cx, |app, cx| {
        assert_eq!(app.input.read(cx).value().as_str(), "new draft");
        let snapshot = app.selected_runtime.read(cx).snapshot();
        assert!(
            snapshot
                .view
                .conversation
                .messages
                .iter()
                .any(|message| message.text == "accepted message")
        );
    });
    close_test_window(view, cx);
    std::fs::remove_dir_all(root).unwrap();
}

#[gpui_kit::test]
fn composer_primary_button_tracks_running_and_draft_content(cx: &mut gpui_kit::TestAppContext) {
    let root = std::env::temp_dir().join(format!("castle-composer-buttons-{}", SessionId::new()));
    let (startup, _) = crate::bootstrap::desktop_startup(root.clone()).unwrap();
    cx.update(crate::bootstrap::init_ui);
    let (view, cx) = cx.add_window_view(|window, cx| DesktopApp::new(startup, window, cx));
    cx.simulate_resize(gpui_kit::size(px(1180.0), px(720.0)));
    for running in [false, true] {
        for text in ["", "  ", "next task"] {
            cx.update(|window, cx| {
                view.update(cx, |app, cx| {
                    app.core.run = if running {
                        RunState::Running {
                            run: crate::session::RunId::default(),
                        }
                    } else {
                        RunState::Idle
                    };
                    app.input
                        .update(cx, |input, cx| input.set_value(text, window, cx));
                    cx.notify();
                })
            });
            cx.run_until_parked();
            assert_eq!(
                cx.debug_bounds("stop").is_some(),
                running && text.trim().is_empty()
            );
            assert_eq!(
                cx.debug_bounds("send").is_some(),
                !running || !text.trim().is_empty()
            );
        }
    }
    close_test_window(view, cx);
    std::fs::remove_dir_all(root).unwrap();
}

#[gpui_kit::test]
fn framework_controls_own_navigation_and_modal_dismissal(cx: &mut gpui_kit::TestAppContext) {
    use crate::app::ComposerMenu;
    let executor = tokio::runtime::Runtime::new().unwrap();
    let _entered = executor.enter();
    cx.background_executor.allow_parking();
    let root = std::env::temp_dir().join(format!(
        "castle-framework-controls-{}",
        harness::SessionId::new()
    ));
    let (startup, _) = crate::bootstrap::desktop_startup(root.clone()).unwrap();
    cx.update(crate::bootstrap::init_ui);
    let (view, cx) = cx.add_window_view(|window, cx| DesktopApp::new(startup, window, cx));
    cx.simulate_resize(gpui_kit::size(px(1180.0), px(720.0)));
    cx.run_until_parked();
    cx.update(|window, cx| {
        view.update(cx, |app, cx| {
            app.open_composer_menu(ComposerMenu::Permission, window, cx)
        })
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("down down enter");
    let deadline = Instant::now() + std::time::Duration::from_secs(5);
    while !view.read_with(cx, |app, cx| {
        app.selected_runtime.read(cx).snapshot().allow_all_tools
    }) {
        assert!(
            Instant::now() < deadline,
            "permission command was not acknowledged"
        );
        cx.run_until_parked();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    view.read_with(cx, |app, cx| {
        assert!(app.selected_runtime.read(cx).snapshot().allow_all_tools);
        assert!(app.core.composer.menu.is_none());
    });
    cx.update(|window, cx| {
        view.update(cx, |app, cx| {
            app.open_composer_menu(ComposerMenu::Model, window, cx)
        })
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    view.read_with(cx, |app, _| assert!(app.core.composer.menu.is_none()));
    cx.update(|window, cx| view.update(cx, |app, cx| app.open_settings_dialog(window, cx)));
    cx.run_until_parked();
    cx.simulate_keystrokes("tab");
    cx.update(|window, cx| assert!(view.read(cx).modal_focus.contains_focused(window, cx)));
    cx.simulate_resize(gpui_kit::size(px(720.0), px(720.0)));
    cx.run_until_parked();
    let bounds = cx.debug_bounds("settings-dialog").unwrap();
    assert!(
        bounds.left() >= px(0.0) && bounds.right() <= px(720.0),
        "settings and its close button must fit the narrow window: {bounds:?}"
    );
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    view.read_with(cx, |app, _| assert!(app.modal.is_none()));
    cx.update(|window, cx| {
        view.update(cx, |app, cx| {
            app.open_target_rename_session_dialog(
                0,
                root.join("rename-placeholder"),
                "   ".into(),
                window,
                cx,
            );
        })
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    view.read_with(cx, |app, _| {
        assert!(app.modal.is_some(), "empty title must not submit")
    });
    let popup = cx.debug_bounds("modal-content").unwrap();
    assert!(
        (popup.center().x - px(360.0)).abs() <= px(1.0)
            && (popup.center().y - px(360.0)).abs() <= px(1.0),
        "the framework must center the popup: {popup:?}"
    );
    cx.simulate_click(popup.origin + point(px(24.0), px(24.0)), Default::default());
    cx.run_until_parked();
    view.read_with(cx, |app, _| {
        assert!(app.modal.is_some(), "popup press must not dismiss")
    });
    cx.simulate_click(point(px(1.0), px(1.0)), Default::default());
    cx.run_until_parked();
    view.read_with(cx, |app, _| assert!(app.modal.is_none()));
    close_test_window(view, cx);
    std::fs::remove_dir_all(root).unwrap();
}

#[gpui_kit::test]
fn session_search_uses_a_dialog_without_replacing_the_workspace_header(
    cx: &mut gpui_kit::TestAppContext,
) {
    let root = std::env::temp_dir().join(format!(
        "castle-session-search-dialog-{}",
        harness::SessionId::new()
    ));
    let (startup, _) = crate::bootstrap::desktop_startup(root.clone()).unwrap();
    cx.update(crate::bootstrap::init_ui);
    let (view, cx) = cx.add_window_view(|window, cx| DesktopApp::new(startup, window, cx));
    cx.simulate_resize(gpui_kit::size(px(1180.0), px(720.0)));
    cx.run_until_parked();
    let workspace_header = cx.debug_bounds("workspace-header").unwrap();

    cx.update(|window, cx| {
        view.update(cx, |app, cx| {
            let project = app.project_store.project(0).unwrap().clone();
            let current_session = app.core.session.current.clone();
            app.project_sessions.insert(
                project.sessions_dir,
                vec![
                    SessionInfo {
                        id: SessionId::new(),
                        project_id: project.id.as_str().to_owned(),
                        path: root.join("older-session"),
                        title: "Older session".into(),
                        created_at: 1,
                        updated_at: 1,
                    },
                    SessionInfo {
                        id: SessionId::new(),
                        project_id: project.id.as_str().to_owned(),
                        path: current_session,
                        title: "Newer session".into(),
                        created_at: 2,
                        updated_at: 2,
                    },
                ],
            );
            app.open_session_search_dialog(window, cx);
        })
    });
    cx.run_until_parked();

    assert_eq!(cx.debug_bounds("workspace-header"), Some(workspace_header));
    let dialog = cx.debug_bounds("session-search-dialog").unwrap();
    assert_eq!(dialog.top(), px(96.0));
    cx.update(|window, cx| {
        assert!(view.read(cx).modal_focus.contains_focused(window, cx));
    });

    cx.simulate_keystrokes("down");
    cx.run_until_parked();
    view.read_with(cx, |app, _| {
        assert!(matches!(
            app.modal,
            Some(Modal::SessionSearch { selected: 1 })
        ));
    });

    cx.simulate_input("missing");
    cx.run_until_parked();
    view.read_with(cx, |app, _| {
        assert!(matches!(
            app.modal,
            Some(Modal::SessionSearch { selected: 0 })
        ));
    });

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    view.read_with(cx, |app, cx| {
        assert!(app.modal.is_none());
        assert!(app.session_search.read(cx).value().is_empty());
    });

    cx.update(|window, cx| view.update(cx, |app, cx| app.open_session_search_dialog(window, cx)));
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    view.read_with(cx, |app, _| assert!(app.modal.is_none()));

    close_test_window(view, cx);
    std::fs::remove_dir_all(root).unwrap();
}

#[gpui_kit::test]
fn sidebar_options_stay_aligned_with_trigger(cx: &mut gpui_kit::TestAppContext) {
    let root = std::env::temp_dir().join(format!(
        "castle-sidebar-options-{}",
        harness::SessionId::new()
    ));
    let (startup, _) = crate::bootstrap::desktop_startup(root.clone()).unwrap();
    cx.update(crate::bootstrap::init_ui);
    let (view, cx) = cx.add_window_view(|window, cx| DesktopApp::new(startup, window, cx));
    cx.simulate_resize(gpui_kit::size(px(1180.0), px(720.0)));
    cx.run_until_parked();
    let trigger = cx.debug_bounds("sidebar-options-trigger").unwrap();
    cx.simulate_click(trigger.center(), Default::default());
    cx.run_until_parked();
    view.read_with(cx, |app, _| assert!(app.core.sidebar.options_open));
    let menu = cx.debug_bounds("sidebar-options").unwrap();
    let gap = menu.left() - trigger.right();
    assert!(gap >= px(0.0) && gap <= px(8.0), "menu gap: {gap:?}");
    assert!(
        (menu.top() - trigger.top()).abs() <= px(8.0),
        "menu: {menu:?}, trigger: {trigger:?}"
    );
    let all_sessions = cx.debug_bounds("group-all-sessions").unwrap();
    cx.simulate_click(all_sessions.center(), Default::default());
    cx.run_until_parked();
    view.read_with(cx, |app, _| {
        assert!(!app.core.sidebar.group_by_workspace);
        assert!(!app.core.sidebar.options_open);
    });
    close_test_window(view, cx);
    std::fs::remove_dir_all(root).unwrap();
}

#[gpui_kit::test]
fn trajectory_scroll_callback_can_leave_and_rejoin_tail(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::{IntoElement, ScrollDelta, Styled, div, list, size};

    struct TestList(ListState);
    impl gpui_kit::Render for TestList {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            list(self.0.clone(), |_, _, _| {
                div().h(px(20.0)).w_full().into_any_element()
            })
            .size_full()
        }
    }

    let root = std::env::temp_dir().join(format!(
        "castle-trajectory-scroll-{}-{}",
        std::process::id(),
        now_ms()
    ));
    let (startup, _) = crate::bootstrap::desktop_startup(root.clone()).unwrap();
    cx.update(crate::bootstrap::init_ui);
    let (view, cx) = cx.add_window_view(|window, cx| {
        let app = DesktopApp::new(startup, window, cx);
        window.blur(cx);
        app
    });
    let state = cx.read_entity(&view, |app, _| app.trajectory_scroll.clone());
    state.reset_with_uniform_height(5, px(20.0));
    state.scroll_to(ListOffset {
        item_ix: 2,
        offset_in_item: px(0.0),
    });

    for (delta, follows) in [(10.0, false), (-10.0, true)] {
        cx.draw(
            point(px(0.0), px(0.0)),
            size(px(100.0), px(60.0)),
            |_, cx| cx.new(|_| TestList(state.clone())).into_any_element(),
        );
        // Exercise GPUI's real callback while its ListState is mutably borrowed.
        cx.simulate_event(ScrollWheelEvent {
            position: point(px(50.0), px(30.0)),
            delta: ScrollDelta::Pixels(point(px(0.0), px(delta))),
            ..Default::default()
        });
        cx.read_entity(&view, |app, _| {
            assert_eq!(app.trajectory_follow_tail.get(), follows);
        });
    }

    close_test_window(view, cx);
    std::fs::remove_dir_all(root).unwrap();
}

fn create_v2_session(
    directory: &Path,
    project_id: &str,
    id: SessionId,
    title: Option<&str>,
) -> SessionInfo {
    create_v2_session_with_config(
        directory,
        project_id,
        id,
        title,
        SessionConfig {
            model: SessionModelConfig {
                model_id: Some("test/test-model".into()),
                reasoning_effort: None,
            },
            allow_all_tools: false,
        },
    )
}

fn create_v2_session_with_config(
    directory: &Path,
    project_id: &str,
    id: SessionId,
    title: Option<&str>,
    config: SessionConfig,
) -> SessionInfo {
    let directory = directory.to_owned();
    let project_id = project_id.to_owned();
    let title = title.map(ToOwned::to_owned);
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(async move {
                let mut session =
                    Session::create_in_project_with_id(directory, project_id, config, id)
                        .await
                        .unwrap();
                if let Some(title) = title {
                    session.rename(&title).await.unwrap();
                }
                session.info().clone()
            })
    })
    .join()
    .unwrap()
}

fn text_stream_model(text: &str) -> (Model, std::thread::JoinHandle<()>) {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let text = text.to_owned();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        let (body_start, content_length) = loop {
            let mut chunk = [0_u8; 4_096];
            let bytes = socket.read(&mut chunk).unwrap();
            assert_ne!(bytes, 0, "client closed before sending request headers");
            request.extend_from_slice(&chunk[..bytes]);
            let Some(header_end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") else {
                continue;
            };
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then_some(value.trim())
                })
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap();
            break (header_end + 4, content_length);
        };
        while request.len() < body_start + content_length {
            let mut chunk = [0_u8; 4_096];
            let bytes = socket.read(&mut chunk).unwrap();
            assert_ne!(bytes, 0, "client closed before sending request body");
            request.extend_from_slice(&chunk[..bytes]);
        }

        let delta = serde_json::json!({
            "type": "response.output_text.delta",
            "sequence_number": 1,
            "item_id": "msg_1",
            "output_index": 0,
            "content_index": 0,
            "delta": text.clone(),
        });
        let completed = serde_json::json!({
            "type": "response.completed",
            "sequence_number": 2,
            "response": {
                "created_at": 0,
                "id": "resp_external",
                "model": "test-model",
                "object": "response",
                "output": [{
                    "type": "message",
                    "content": [{
                        "type": "output_text",
                        "annotations": [],
                        "text": text,
                    }],
                    "id": "msg_1",
                    "role": "assistant",
                    "status": "completed",
                }],
                "status": "completed",
            },
        });
        let body = format!("data: {delta}\n\ndata: {completed}\n\n");
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        socket.write_all(response.as_bytes()).unwrap();
    });
    (
        Model::new(
            "test",
            "key",
            format!("http://{address}"),
            "test-model",
            10_000,
        ),
        server,
    )
}

fn commit_external_turn(path: &Path, project_id: &str, input: &str, output: &str) -> u64 {
    let path = path.to_owned();
    let project_id = project_id.to_owned();
    let input = input.to_owned();
    let (model, server) = text_stream_model(output);
    let runner = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async move {
                let session = Session::open_in_project(path, &project_id).await.unwrap();
                let agent = SessionSetup::new(model, "test", session, ".");
                let host =
                    harness::SessionHandle::new(agent, project_id, None, SessionConfig::default());
                let mut connection = host.connect().unwrap();
                host.send(harness::SessionCommand::Submit {
                    id: harness::InputId::random(),
                    text: input,
                    mode: harness::SubmitMode::Start,
                })
                .await
                .unwrap();
                loop {
                    if let harness::SessionUpdate::Changed(snapshot) =
                        connection.events.recv().await.unwrap()
                        && !snapshot.status.is_active()
                    {
                        assert!(matches!(snapshot.status, harness::RuntimeStatus::Idle));
                        break snapshot.revision;
                    }
                }
            })
    });
    let revision = runner.join().unwrap();
    server.join().unwrap();
    revision
}

fn persist_external_config(path: &Path, project_id: &str, config: SessionConfig) {
    let path = path.to_owned();
    let project_id = project_id.to_owned();
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async move {
                let session = Session::open_in_project(path, &project_id).await.unwrap();
                let agent = SessionSetup::new(
                    Model::new("test", "key", "http://localhost", "test-model", 10_000),
                    "test",
                    session,
                    ".",
                );
                let host = harness::SessionHandle::new(agent, project_id, None, config.clone());
                host.send(harness::SessionCommand::Configure {
                    config,
                    model: None,
                })
                .await
                .unwrap();
            });
    })
    .join()
    .unwrap();
}

#[test]
fn composer_submits_only_unshifted_enter_events() {
    assert!(is_composer_submit_event(&InputEvent::PressEnter {
        secondary: false,
        shift: false,
    }));
    assert!(!is_composer_submit_event(&InputEvent::PressEnter {
        secondary: false,
        shift: true,
    }));
    assert!(!is_composer_submit_event(&InputEvent::Change));
}

#[test]
fn composer_models_require_configured_credentials() {
    let models = [
        ConfiguredModel::new(
            "deepseek-official",
            ProviderModel::new("deepseek-test", "DeepSeek Test", 10_000, None),
            Model::new("DeepSeek", "", "http://localhost", "deepseek-test", 10_000),
        ),
        ConfiguredModel::new(
            "openai",
            ProviderModel::new("gpt-test", "GPT Test", 10_000, None),
            Model::new("OpenAI", "secret", "http://localhost", "gpt-test", 10_000),
        ),
        ConfiguredModel::new(
            "openai",
            ProviderModel::new("gpt-test-2", "GPT Test 2", 10_000, None),
            Model::new("OpenAI", "secret", "http://localhost", "gpt-test-2", 10_000),
        ),
    ];

    assert_eq!(
        composer_model_indices(&models).collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(
        active_model_index(&models, Some("deepseek-official/deepseek-test")),
        Some(1)
    );
}

#[test]
fn invalid_session_open_errors_are_silent_but_operational_errors_are_reported() {
    let invalid_locator = SessionError::Invalid("bad locator".into());
    let corrupt_history = SessionError::Store(SessionStoreError::Corrupt("bad tail".into()));
    let missing_database = SessionError::Io(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "missing database",
    ));
    assert!(session_open_error_notice(&invalid_locator).is_none());
    assert!(session_open_error_notice(&corrupt_history).is_none());
    assert!(session_open_error_notice(&missing_database).is_none());

    let writer_busy = SessionError::Store(SessionStoreError::WriterBusy {
        session_id: SessionId::from_raw("busy-session"),
    });
    assert!(
        session_open_error_notice(&writer_busy)
            .is_some_and(|notice| notice.contains("already has an active writer"))
    );
}

#[test]
fn catalog_cache_policy_clears_invalid_data_but_retains_transient_failures() {
    let invalid = SessionError::Store(SessionStoreError::UnsupportedSchemaVersion {
        found: 5,
        expected: 6,
    });
    let transient = SessionError::Io(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        "database temporarily unavailable",
    ));
    let operational = SessionError::Store(SessionStoreError::ReadonlyStore);
    assert!(should_clear_catalog_after_error(&invalid));
    assert!(!should_clear_catalog_after_error(&transient));
    assert!(!should_clear_catalog_after_error(&operational));
}

#[test]
fn transient_and_operational_catalog_failures_retain_every_cached_projection() {
    let project_id = ProjectId::default_project();
    let sessions_dir = PathBuf::from("sessions");
    let path = sessions_dir.join("retained.session-v2");
    let session_id = SessionId::from_raw("retained");
    let session = SessionInfo {
        id: session_id.clone(),
        project_id: project_id.as_str().into(),
        path: path.clone(),
        title: "Retained".into(),
        created_at: 0,
        updated_at: 0,
    };
    let errors = [
        SessionError::Io(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "database temporarily unavailable",
        )),
        SessionError::Store(SessionStoreError::ReadonlyStore),
    ];

    for error in errors {
        let mut sessions = HashMap::from([(sessions_dir.clone(), vec![session.clone()])]);
        let mut documents = HashMap::from([(
            path.clone(),
            session_search_document(Arc::from(["retained".into()])),
        )]);
        let mut indices = HashMap::from([((project_id.clone(), session_id.clone()), 0)]);

        assert!(!apply_project_catalog_result(
            &project_id,
            &sessions_dir,
            Err(error),
            &mut sessions,
            &mut documents,
            &mut indices,
        ));

        assert_eq!(sessions[&sessions_dir], vec![session.clone()]);
        assert_eq!(documents[&path].searchable.as_ref(), "retained");
        assert_eq!(indices[&(project_id.clone(), session_id.clone())], 0);
    }
}

#[test]
fn startup_catalog_loader_reads_once_and_builds_consistent_linear_projections() {
    const SESSIONS_PER_PROJECT: usize = 3_334;

    let root = std::env::temp_dir().join(format!(
        "castle-desktop-linear-catalog-{}-{}",
        std::process::id(),
        now_ms()
    ));
    let workspace_a = root.join("workspace-a");
    let workspace_b = root.join("workspace-b");
    std::fs::create_dir_all(&workspace_a).unwrap();
    std::fs::create_dir_all(&workspace_b).unwrap();
    let (mut project_store, _) = ProjectStore::load(root.join("state"), Some(workspace_a)).unwrap();
    project_store.add(workspace_b).unwrap();
    let project_count = project_store.projects().len();
    let mut reads = HashMap::<String, usize>::new();

    let cache =
        load_session_catalog_cache_with(&project_store, |directory: &Path, project_id: &str| {
            *reads.entry(project_id.to_owned()).or_default() += 1;
            let mut catalog = SessionCatalog {
                sessions: Vec::with_capacity(SESSIONS_PER_PROJECT),
                search_values: HashMap::with_capacity(SESSIONS_PER_PROJECT),
            };
            for index in 0..SESSIONS_PER_PROJECT {
                let raw_id = format!("{project_id}-{index}");
                let id = SessionId::from_raw(raw_id.clone());
                let title = format!("Session {raw_id}");
                let path = directory.join(format!("{raw_id}.session-v2"));
                catalog.sessions.push(SessionInfo {
                    id,
                    project_id: project_id.into(),
                    path: path.clone(),
                    title: title.clone(),
                    created_at: index as u64,
                    updated_at: index as u64,
                });
                catalog
                    .search_values
                    .insert(path, Arc::from([title.clone()]));
            }
            Ok(catalog)
        });

    let expected_sessions = project_count * SESSIONS_PER_PROJECT;
    assert!(expected_sessions >= 10_000);
    assert_eq!(reads.len(), project_count);
    assert!(reads.values().all(|reads| *reads == 1));
    assert_eq!(cache.project_sessions.len(), project_count);
    assert_eq!(cache.session_search_documents.len(), expected_sessions);
    assert_eq!(cache.session_catalog_indices.len(), expected_sessions);
    for project in project_store.projects() {
        let sessions = &cache.project_sessions[&project.sessions_dir];
        assert_eq!(sessions.len(), SESSIONS_PER_PROJECT);
        for (index, session) in sessions.iter().enumerate() {
            assert_eq!(session.project_id, project.id.as_str());
            assert_eq!(
                cache
                    .session_catalog_indices
                    .get(&(project.id.clone(), session.id.clone())),
                Some(&index)
            );
            assert!(cache.session_search_documents.contains_key(&session.path));
        }
    }

    drop(project_store);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn clearing_an_invalid_project_catalog_removes_all_project_projections_only() {
    let project_id = ProjectId::default_project();
    let other_project_id: ProjectId = serde_json::from_str("\"other-project\"").unwrap();
    let sessions_dir = PathBuf::from("sessions");
    let other_sessions_dir = PathBuf::from("other-sessions");
    let invalid_path = sessions_dir.join("invalid.session-v2");
    let other_path = other_sessions_dir.join("valid.session-v2");
    let invalid_id = SessionId::from_raw("invalid");
    let other_id = SessionId::from_raw("valid");
    let session = |id: SessionId, project: &ProjectId, path: PathBuf| SessionInfo {
        id,
        project_id: project.as_str().into(),
        path,
        title: "Session".into(),
        created_at: 0,
        updated_at: 0,
    };
    let mut sessions = HashMap::from([
        (
            sessions_dir.clone(),
            vec![session(
                invalid_id.clone(),
                &project_id,
                invalid_path.clone(),
            )],
        ),
        (
            other_sessions_dir.clone(),
            vec![session(
                other_id.clone(),
                &other_project_id,
                other_path.clone(),
            )],
        ),
    ]);
    let document = || session_search_document(Arc::from(["needle".into()]));
    let mut documents = HashMap::from([
        (invalid_path.clone(), document()),
        (other_path.clone(), document()),
    ]);
    let mut indices = HashMap::from([
        ((project_id.clone(), invalid_id), 0),
        ((other_project_id.clone(), other_id), 0),
    ]);

    clear_project_catalog_cache(
        &project_id,
        &sessions_dir,
        &mut sessions,
        &mut documents,
        &mut indices,
    );

    assert!(sessions[&sessions_dir].is_empty());
    assert_eq!(sessions[&other_sessions_dir].len(), 1);
    assert!(!documents.contains_key(&invalid_path));
    assert!(documents.contains_key(&other_path));
    assert!(!indices.keys().any(|(project, _)| project == &project_id));
    assert!(
        indices
            .keys()
            .any(|(project, _)| project == &other_project_id)
    );
}

#[gpui_kit::test]
fn refreshing_an_invalid_catalog_removes_stale_sidebar_search_and_index_rows(
    cx: &mut gpui_kit::TestAppContext,
) {
    let root = std::env::temp_dir().join(format!(
        "castle-desktop-invalid-switch-{}-{}",
        std::process::id(),
        now_ms()
    ));
    let workspace_a = root.join("workspace-a");
    let workspace_b = root.join("workspace-b");
    std::fs::create_dir_all(&workspace_a).unwrap();
    std::fs::create_dir_all(&workspace_b).unwrap();
    let (mut project_store, active_project) =
        ProjectStore::load(root.join("state"), Some(workspace_a.clone())).unwrap();
    let project_b_index = project_store.add(workspace_b).unwrap();
    let project_b = project_store.project(project_b_index).unwrap().clone();
    let invalid_id = SessionId::from_raw("stale-invalid-switch");
    let invalid_path = project_b
        .sessions_dir
        .join(format!("{invalid_id}.session-v2"));
    let invalid = SessionInfo {
        id: invalid_id.clone(),
        project_id: project_b.id.as_str().into(),
        path: invalid_path.clone(),
        title: "Stale invalid session".into(),
        created_at: 0,
        updated_at: 0,
    };
    let settings = SettingsStore::load(root.join("settings")).unwrap();
    let model = Model::new("test", "key", "http://localhost", "test-model", 10_000);
    let profile = ProviderModel::new("test-model", "Test Model", 10_000, None);
    let configured = ConfiguredModel::new("test", profile, model.clone());
    let agent = SessionSetup::new(model, "test", Session::memory(), workspace_a);

    cx.update(crate::bootstrap::init_ui);
    let (view, cx) = cx.add_window_view(|window, cx| {
        let app = DesktopApp::new(
            DesktopStartup {
                agent,
                models: vec![configured],
                selected_model: 0,
                project_store,
                active_project,
                settings,
            },
            window,
            cx,
        );
        window.blur(cx);
        app
    });

    std::fs::write(
        project_b.sessions_dir.join(harness::SESSION_DATABASE_FILE),
        b"not a sqlite database",
    )
    .unwrap();
    cx.update(|_, app| {
        view.update(app, |this, _| {
            this.project_sessions
                .get_mut(&project_b.sessions_dir)
                .unwrap()
                .push(invalid.clone());
            this.session_search_documents.insert(
                invalid_path.clone(),
                session_search_document(Arc::from(["stale".into()])),
            );
            this.session_catalog_indices
                .insert((project_b.id.clone(), invalid_id.clone()), 0);

            assert!(!this.refresh_project_catalog(&project_b.id));
        });
    });

    cx.read_entity(&view, |app, _| {
        assert!(app.project_sessions[&project_b.sessions_dir].is_empty());
        assert!(!app.session_search_documents.contains_key(&invalid_path));
        assert!(
            !app.session_catalog_indices
                .contains_key(&(project_b.id.clone(), invalid_id))
        );
    });

    close_test_window(view, cx);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn removing_an_invalid_catalog_entry_reindexes_remaining_entries() {
    let project_id = ProjectId::default_project();
    let sessions_dir = PathBuf::from("sessions");
    let first_path = sessions_dir.join("first.session-v2");
    let second_path = sessions_dir.join("second.session-v2");
    let first = SessionInfo {
        id: SessionId::from_raw("first"),
        project_id: project_id.as_str().into(),
        path: first_path.clone(),
        title: "First".into(),
        created_at: 0,
        updated_at: 0,
    };
    let second = SessionInfo {
        id: SessionId::from_raw("second"),
        project_id: project_id.as_str().into(),
        path: second_path.clone(),
        title: "Second".into(),
        created_at: 0,
        updated_at: 0,
    };
    let mut sessions = HashMap::from([(sessions_dir.clone(), vec![first.clone(), second.clone()])]);
    let mut documents = HashMap::from([
        (
            first_path.clone(),
            session_search_document(Arc::from(["first".into()])),
        ),
        (
            second_path.clone(),
            session_search_document(Arc::from(["second".into()])),
        ),
    ]);
    let mut indices = HashMap::from([
        ((project_id.clone(), first.id.clone()), 0),
        ((project_id.clone(), second.id.clone()), 1),
    ]);

    assert_eq!(
        remove_session_catalog_entry(
            &project_id,
            &sessions_dir,
            &first_path,
            &mut sessions,
            &mut documents,
            &mut indices,
        ),
        Some(first.id)
    );
    assert_eq!(sessions[&sessions_dir], vec![second.clone()]);
    assert!(!documents.contains_key(&first_path));
    assert!(documents.contains_key(&second_path));
    assert_eq!(indices.get(&(project_id, second.id)), Some(&0));
}

#[gpui_kit::test]
fn runtime_creation_preserves_defaults_and_persists_model_fallback(
    cx: &mut gpui_kit::TestAppContext,
) {
    use harness::ReasoningEffort;

    let executor = tokio::runtime::Runtime::new().unwrap();
    let _entered = executor.enter();
    cx.background_executor.allow_parking();
    let root = std::env::temp_dir().join(format!("castle-runtime-config-{}", SessionId::new()));
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let (project_store, active_project) =
        ProjectStore::load(root.join("state"), Some(workspace.clone())).unwrap();
    let project = project_store.project(active_project).unwrap().clone();
    let mut settings = SettingsStore::load(root.join("settings")).unwrap();
    settings.set_allow_all_tools(true).unwrap();
    let first = ConfiguredModel::new(
        "first",
        ProviderModel::new("first-model", "First", 10_000, None),
        Model::new("first", "key", "http://127.0.0.1:1", "first-model", 10_000),
    );
    let (model, server) = text_stream_model("done");
    let mut selected = ConfiguredModel::new(
        "selected",
        ProviderModel::new("test-model", "Selected", 10_000, None),
        model.with_reasoning_efforts(&[ReasoningEffort::High]),
    );
    selected.reasoning_effort = Some(ReasoningEffort::High);
    let expected = config_for_model(&selected, true);
    let agent = SessionSetup::new(
        selected.model.clone(),
        "test",
        Session::memory(),
        &workspace,
    );
    cx.update(crate::bootstrap::init_ui);
    let (view, cx) = cx.add_window_view(|window, cx| {
        let app = DesktopApp::new(
            DesktopStartup {
                agent,
                models: vec![first, selected],
                selected_model: 1,
                project_store,
                active_project,
                settings,
            },
            window,
            cx,
        );
        window.blur(cx);
        app
    });

    // Exercise the creation route used after the startup draft has been submitted.
    let draft = cx.update(|_, app| {
        view.update(app, |this, cx| {
            this.create_runtime(active_project, Session::memory(), cx)
                .unwrap()
        })
    });
    cx.read_entity(&draft, |runtime, _| {
        assert_eq!(runtime.snapshot().config, expected)
    });
    cx.update(|window, app| {
        draft.update(app, |runtime, cx| {
            runtime.submit("hello".into(), window, cx);
        });
    });
    let deadline = Instant::now() + std::time::Duration::from_secs(10);
    while cx.read_entity(&draft, |runtime, _| runtime.is_active()) {
        assert!(Instant::now() < deadline, "draft submission did not settle");
        cx.run_until_parked();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    server.join().unwrap();
    let snapshot = cx.read_entity(&draft, |runtime, _| runtime.snapshot());
    assert_eq!(snapshot.status, SessionConnectionStatus::Idle);
    let stored = Session::inspect(&snapshot.session.path).unwrap();
    assert_eq!(stored.config(), &expected);
    assert!(stored.events().iter().any(|recorded| matches!(
        &recorded.event,
        harness::SessionEvent::RequestSnapshot { model, reasoning_effort, session_config, .. }
            if model == "test-model"
                && *reasoning_effort == Some(ReasoningEffort::High)
                && session_config == &expected
    )));
    drop(stored);

    // Missing model IDs must be repaired durably without inheriting global permissions.
    let unavailable = SessionConfig {
        model: SessionModelConfig {
            model_id: Some("removed/model".into()),
            reasoning_effort: Some(ReasoningEffort::High),
        },
        allow_all_tools: false,
    };
    let info = create_v2_session_with_config(
        &project.sessions_dir,
        project.id.as_str(),
        SessionId::new(),
        None,
        unavailable,
    );
    let loaded = Session::open_writable_in_project(&info.path, project.id.as_str()).unwrap();
    let fallback = cx.update(|_, app| {
        view.update(app, |this, cx| {
            this.create_runtime(active_project, loaded, cx).unwrap()
        })
    });
    let deadline = Instant::now() + std::time::Duration::from_secs(10);
    while cx.read_entity(&fallback, |runtime, _| runtime.is_active()) {
        assert!(
            Instant::now() < deadline,
            "fallback configuration did not settle"
        );
        cx.run_until_parked();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let repaired = Session::open_writable_in_project(&info.path, project.id.as_str()).unwrap();
    cx.read_entity(&fallback, |runtime, _| {
        let snapshot = runtime.snapshot();
        assert_eq!(snapshot.status, SessionConnectionStatus::Idle);
        assert_eq!(
            snapshot.config.model.model_id.as_deref(),
            Some("first/first-model")
        );
        assert_eq!(snapshot.config.model.reasoning_effort, None);
        assert!(!snapshot.config.allow_all_tools);
        assert_eq!(&snapshot.config, repaired.config());
        assert!(runtime.matches_loaded_session(&repaired));
    });
    drop(repaired);
    drop(fallback);
    drop(draft);
    close_test_window(view, cx);
    std::fs::remove_dir_all(root).unwrap();
}

#[gpui_kit::test]
fn created_session_path_refreshes_the_sidebar_list(cx: &mut gpui_kit::TestAppContext) {
    let root = std::env::temp_dir().join(format!(
        "castle-desktop-new-session-{}-{}",
        std::process::id(),
        now_ms()
    ));
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let (project_store, active_project) =
        ProjectStore::load(root.join("state"), Some(workspace.clone())).unwrap();
    let settings = SettingsStore::load(root.join("settings")).unwrap();
    let model = Model::new("test", "key", "http://localhost", "test-model", 10_000);
    let profile = ProviderModel::new("test-model", "Test Model", 10_000, None);
    let configured = ConfiguredModel::new("test", profile, model.clone());
    let agent = SessionSetup::new(model, "test", Session::memory(), workspace);

    cx.update(crate::bootstrap::init_ui);
    let (view, cx) = cx.add_window_view(|window, cx| {
        let app = DesktopApp::new(
            DesktopStartup {
                agent,
                models: vec![configured],
                selected_model: 0,
                project_store,
                active_project,
                settings,
            },
            window,
            cx,
        );
        window.blur(cx);
        app
    });

    cx.update(|_, app| {
        view.update(app, |this, cx| {
            let mut snapshot = this.selected_runtime.read(cx).snapshot();
            let project_id = this
                .project_store
                .project(this.core.workspace.active_project)
                .unwrap()
                .id
                .as_str()
                .to_owned();
            snapshot.session = create_v2_session(
                &this.core.workspace.sessions_dir,
                &project_id,
                snapshot.session.id.clone(),
                None,
            );
            this.apply_selected_runtime_snapshot(snapshot);
        });
    });

    cx.read_entity(&view, |app, _| {
        assert!(!app.core.session.current.as_os_str().is_empty());
        assert_eq!(
            app.core
                .session
                .current
                .extension()
                .and_then(|value| value.to_str()),
            Some("session-v2")
        );
        assert!(!app.core.session.current.exists());
        assert!(
            app.core
                .workspace
                .sessions_dir
                .join(harness::SESSION_DATABASE_FILE)
                .is_file()
        );
        assert_eq!(
            app.project_sessions[&app.core.workspace.sessions_dir].len(),
            1
        );
        assert_eq!(
            app.project_sessions[&app.core.workspace.sessions_dir][0].path,
            app.core.session.current
        );
    });

    close_test_window(view, cx);
    std::fs::remove_dir_all(root).unwrap();
}

#[gpui_kit::test]
fn opening_an_invalid_session_silently_removes_its_catalog_entry(
    cx: &mut gpui_kit::TestAppContext,
) {
    let root = std::env::temp_dir().join(format!(
        "castle-desktop-invalid-open-{}-{}",
        std::process::id(),
        now_ms()
    ));
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let (project_store, active_project) =
        ProjectStore::load(root.join("state"), Some(workspace.clone())).unwrap();
    let project = project_store.project(active_project).unwrap().clone();
    let invalid = SessionInfo {
        id: SessionId::from_raw("invalid-open"),
        project_id: project.id.as_str().into(),
        path: project.sessions_dir.join("invalid-open.not-a-session"),
        title: "Invalid".into(),
        created_at: 0,
        updated_at: 0,
    };
    let settings = SettingsStore::load(root.join("settings")).unwrap();
    let model = Model::new("test", "key", "http://localhost", "test-model", 10_000);
    let profile = ProviderModel::new("test-model", "Test Model", 10_000, None);
    let configured = ConfiguredModel::new("test", profile, model.clone());
    let agent = SessionSetup::new(model, "test", Session::memory(), workspace);

    cx.update(crate::bootstrap::init_ui);
    let (view, cx) = cx.add_window_view(|window, cx| {
        let app = DesktopApp::new(
            DesktopStartup {
                agent,
                models: vec![configured],
                selected_model: 0,
                project_store,
                active_project,
                settings,
            },
            window,
            cx,
        );
        window.blur(cx);
        app
    });

    cx.update(|window, app| {
        view.update(app, |this, cx| {
            this.project_sessions
                .get_mut(&project.sessions_dir)
                .unwrap()
                .push(invalid.clone());
            this.session_catalog_indices
                .insert((project.id.clone(), invalid.id.clone()), 0);
            this.session_search_documents.insert(
                invalid.path.clone(),
                session_search_document(Arc::from(["invalid".into()])),
            );
            let generation = this.begin_runtime_selection_intent();
            let key = (project.id.clone(), invalid.path.clone());
            assert!(this.start_session_open_request(&key, generation));
            assert_eq!(
                this.finish_session_open_request(&key, generation, &project.id),
                SessionOpenCompletion::Current
            );
            // Exercise the same atomic error branch as `open_session_async` without asking a
            // GPUI test window to perform the real-platform focus operation at its end.
            this.discard_invalid_session_catalog_entry(active_project, &invalid.path);
            assert!(!this.resolve_failed_runtime_selection(
                generation,
                &project.id,
                &invalid.path,
                active_project,
                cx,
            ));
            let _ = window;
        });
    });
    cx.run_until_parked();

    cx.read_entity(&view, |app, _| {
        assert!(!app.selection_pending());
        assert!(app.project_sessions[&project.sessions_dir].is_empty());
        assert!(!app.session_search_documents.contains_key(&invalid.path));
        assert!(app.core.transient_messages.is_empty());
        assert!(app.core.session.current.as_os_str().is_empty());
    });

    close_test_window(view, cx);
    std::fs::remove_dir_all(root).unwrap();
}

#[gpui_kit::test]
fn archive_and_restore_refresh_both_session_catalogs(cx: &mut gpui_kit::TestAppContext) {
    let root = std::env::temp_dir().join(format!(
        "castle-desktop-archive-{}-{}",
        std::process::id(),
        now_ms()
    ));
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let (project_store, active_project) =
        ProjectStore::load(root.join("state"), Some(workspace.clone())).unwrap();
    let project = project_store.project(active_project).unwrap();
    let sessions_dir = project.sessions_dir.clone();
    let session = create_v2_session(
        &sessions_dir,
        project.id.as_str(),
        SessionId::from_raw("archive-test"),
        Some("Archive me"),
    );
    let path = session.path;
    let settings = SettingsStore::load(root.join("settings")).unwrap();
    let model = Model::new("test", "key", "http://localhost", "test-model", 10_000);
    let profile = ProviderModel::new("test-model", "Test Model", 10_000, None);
    let configured = ConfiguredModel::new("test", profile, model.clone());
    let agent = SessionSetup::new(model, "test", Session::memory(), workspace);

    cx.update(crate::bootstrap::init_ui);
    let (view, cx) = cx.add_window_view(|window, cx| {
        let app = DesktopApp::new(
            DesktopStartup {
                agent,
                models: vec![configured],
                selected_model: 0,
                project_store,
                active_project,
                settings,
            },
            window,
            cx,
        );
        window.blur(cx);
        app
    });

    cx.update(|window, app| {
        view.update(app, |this, cx| {
            this.archive_target_session(active_project, path.clone(), window, cx)
        });
    });
    let archived = cx.read_entity(&view, |app, _| {
        assert!(app.project_sessions[&sessions_dir].is_empty());
        assert_eq!(app.project_archived_sessions[&sessions_dir].len(), 1);
        app.project_archived_sessions[&sessions_dir][0].clone()
    });
    assert!(!archived.path.exists());

    cx.update(|_window, app| {
        view.update(app, |this, cx| {
            this.restore_archived_session(active_project, archived.clone(), cx)
        });
    });
    cx.read_entity(&view, |app, _| {
        assert_eq!(app.project_sessions[&sessions_dir].len(), 1);
        assert!(app.project_archived_sessions[&sessions_dir].is_empty());
    });

    close_test_window(view, cx);
    std::fs::remove_dir_all(root).unwrap();
}

#[gpui_kit::test]
fn stale_session_open_cannot_replace_a_new_chat_draft(cx: &mut gpui_kit::TestAppContext) {
    let root = std::env::temp_dir().join(format!(
        "castle-desktop-stale-open-{}-{}",
        std::process::id(),
        now_ms()
    ));
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let (project_store, active_project) =
        ProjectStore::load(root.join("state"), Some(workspace.clone())).unwrap();
    let project = project_store.project(active_project).unwrap().clone();
    let stale_session = create_v2_session(
        &project.sessions_dir,
        project.id.as_str(),
        SessionId::from_raw("stale-open"),
        Some("Stale open"),
    );
    let settings = SettingsStore::load(root.join("settings")).unwrap();
    let model = Model::new("test", "key", "http://localhost", "test-model", 10_000);
    let profile = ProviderModel::new("test-model", "Test Model", 10_000, None);
    let configured = ConfiguredModel::new("test", profile, model.clone());
    let agent = SessionSetup::new(model, "test", Session::memory(), workspace);

    cx.update(crate::bootstrap::init_ui);
    let (view, cx) = cx.add_window_view(|window, cx| {
        let app = DesktopApp::new(
            DesktopStartup {
                agent,
                models: vec![configured],
                selected_model: 0,
                project_store,
                active_project,
                settings,
            },
            window,
            cx,
        );
        window.blur(cx);
        app
    });

    cx.update(|_, app| {
        view.update(app, |this, cx| {
            let session =
                Session::open_writable_in_project(&stale_session.path, project.id.as_str())
                    .unwrap();
            let stale_runtime = this.create_runtime(active_project, session, cx).unwrap();
            let stale_generation = this.begin_runtime_selection_intent();
            let key = (project.id.clone(), stale_session.path.clone());
            this.inflight_session_opens
                .insert(key.clone(), stale_generation);

            assert!(this.select_new_chat_draft(cx));

            let completed_generation = this.inflight_session_opens.remove(&key).unwrap();
            if this.session_open_matches_current_intent(completed_generation, &project.id) {
                this.select_runtime(stale_runtime, cx);
            }
            assert!(
                this.selected_runtime
                    .read(cx)
                    .observation()
                    .session
                    .path
                    .as_os_str()
                    .is_empty()
            );
            assert!(this.core.session.current.as_os_str().is_empty());
        });
    });

    close_test_window(view, cx);
    std::fs::remove_dir_all(root).unwrap();
}

#[gpui_kit::test]
fn pending_session_open_does_not_submit_to_the_previous_runtime(cx: &mut gpui_kit::TestAppContext) {
    let root = std::env::temp_dir().join(format!(
        "castle-desktop-pending-open-command-gate-{}-{}",
        std::process::id(),
        now_ms()
    ));
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let (project_store, active_project) =
        ProjectStore::load(root.join("state"), Some(workspace.clone())).unwrap();
    let project = project_store.project(active_project).unwrap().clone();
    let target = create_v2_session(
        &project.sessions_dir,
        project.id.as_str(),
        SessionId::from_raw("pending-command-target"),
        Some("Pending command target"),
    );
    let settings = SettingsStore::load(root.join("settings")).unwrap();
    let model = Model::new("test", "key", "http://localhost", "test-model", 10_000);
    let profile = ProviderModel::new("test-model", "Test Model", 10_000, None);
    let configured = ConfiguredModel::new("test", profile, model.clone());
    let agent = SessionSetup::new(model, "test", Session::memory(), workspace);

    cx.update(crate::bootstrap::init_ui);
    let (view, cx) = cx.add_window_view(|window, cx| {
        let app = DesktopApp::new(
            DesktopStartup {
                agent,
                models: vec![configured],
                selected_model: 0,
                project_store,
                active_project,
                settings,
            },
            window,
            cx,
        );
        window.blur(cx);
        app
    });

    let loaded_target =
        Session::open_writable_in_project(&target.path, project.id.as_str()).unwrap();
    let (previous_entity, target_entity) = cx.update(|window, app| {
        view.update(app, |this, cx| {
            let previous_entity = this.selected_runtime.entity_id();
            let generation = this.begin_runtime_selection_intent();
            let target_key = (project.id.clone(), target.path.clone());
            assert!(this.start_session_open_request(&target_key, generation));
            assert!(this.selection_pending());
            assert_eq!(this.selected_runtime.entity_id(), previous_entity);

            this.input.update(cx, |input, cx| {
                input.set_value("must wait for target", window, cx)
            });
            this.submit(window, cx);
            this.set_allow_all_tools(true, cx);

            assert_eq!(this.input.read(cx).value(), "must wait for target");
            let previous = this.selected_runtime.read(cx).snapshot();
            assert_eq!(previous.status, SessionConnectionStatus::Idle);
            assert!(!previous.allow_all_tools);
            assert!(previous.view.conversation.messages.is_empty());

            let pending = this.pending_runtime_selection.clone().unwrap();
            let key = (project.id.clone(), target.path.clone());
            assert_eq!(
                this.finish_session_open_request(&key, pending.generation, &project.id,),
                SessionOpenCompletion::Current
            );
            let runtime = this
                .reconcile_loaded_runtime(active_project, loaded_target, cx)
                .unwrap();
            this.select_runtime(runtime, cx);
            assert!(!this.selection_pending());
            assert_ne!(this.selected_runtime.entity_id(), previous_entity);
            assert_eq!(this.core.session.current, target.path);
            assert_eq!(this.input.read(cx).value(), "must wait for target");
            let target_entity = this.selected_runtime.entity_id();

            let missing_path = project
                .sessions_dir
                .join("missing-same-project-session.session-v2");
            let generation = this.begin_runtime_selection_intent();
            let missing_key = (project.id.clone(), missing_path.clone());
            assert!(this.start_session_open_request(&missing_key, generation));
            assert!(this.selection_pending());
            let pending = this.pending_runtime_selection.clone().unwrap();
            let key = (project.id.clone(), missing_path.clone());
            assert_eq!(
                this.finish_session_open_request(&key, pending.generation, &project.id,),
                SessionOpenCompletion::Current
            );
            assert!(!this.resolve_failed_runtime_selection(
                pending.generation,
                &project.id,
                &missing_path,
                active_project,
                cx,
            ));
            assert!(!this.selection_pending());
            assert_eq!(this.selected_runtime.entity_id(), target_entity);
            assert_eq!(this.core.session.current, target.path);
            (previous_entity, target_entity)
        })
    });
    cx.read_entity(&view, |this, _| {
        assert!(!this.selection_pending());
        assert_ne!(target_entity, previous_entity);
        assert_eq!(this.selected_runtime.entity_id(), target_entity);
        assert_eq!(this.core.session.current, target.path);
    });

    close_test_window(view, cx);
    std::fs::remove_dir_all(root).unwrap();
}

#[gpui_kit::test]
fn successful_session_open_captures_view_edits_made_while_loading(
    cx: &mut gpui_kit::TestAppContext,
) {
    let root = std::env::temp_dir().join(format!(
        "castle-desktop-pending-open-view-state-{}-{}",
        std::process::id(),
        now_ms()
    ));
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let (project_store, active_project) =
        ProjectStore::load(root.join("state"), Some(workspace.clone())).unwrap();
    let project = project_store.project(active_project).unwrap().clone();
    let target = create_v2_session(
        &project.sessions_dir,
        project.id.as_str(),
        SessionId::from_raw("pending-view-state-target"),
        Some("Pending view-state target"),
    );
    let settings = SettingsStore::load(root.join("settings")).unwrap();
    let model = Model::new("test", "key", "http://localhost", "test-model", 10_000);
    let profile = ProviderModel::new("test-model", "Test Model", 10_000, None);
    let configured = ConfiguredModel::new("test", profile, model.clone());
    let agent = SessionSetup::new(model, "test", Session::memory(), workspace);

    cx.update(crate::bootstrap::init_ui);
    let (view, cx) = cx.add_window_view(|window, cx| {
        let app = DesktopApp::new(
            DesktopStartup {
                agent,
                models: vec![configured],
                selected_model: 0,
                project_store,
                active_project,
                settings,
            },
            window,
            cx,
        );
        window.blur(cx);
        app
    });

    let loaded_target =
        Session::open_writable_in_project(&target.path, project.id.as_str()).unwrap();
    cx.update(|window, app| {
        view.update(app, |this, cx| {
            let source_key = this
                .runtime_location(&this.selected_runtime)
                .expect("the source draft is registered");
            let source = this.selected_runtime.clone();
            let generation = this.begin_runtime_selection_intent();
            let target_key = (project.id.clone(), target.path.clone());
            assert!(this.start_session_open_request(&target_key, generation));

            // These changes happen after the eager capture performed by the click handler.
            // The successful handoff must capture them once more before replacing the runtime.
            this.set_trajectory(true, window, cx);
            this.trajectory_query_value = "edited while loading".into();
            this.trajectory_follow_tail.set(false);
            let offset = ListOffset {
                item_ix: 7,
                offset_in_item: px(3.0),
            };
            this.trajectory_scroll_restore.set(Some(offset));
            this.core.details.tab_history = vec![DetailsTab::Raw, DetailsTab::Timing];
            this.details_scroll.set_offset(point(px(0.0), px(-42.0)));
            let projection = &this.core.session_view.trajectory;
            let axis = AxisId {
                document_generation: projection.projection_lineage(),
                geometry_revision: projection.revision(),
                mode: this.core.trajectory.mode,
            };
            let selected = AxisRange {
                axis,
                range: DomainRange::new(2.0, 5.0),
            };
            let viewport = AxisRange {
                axis,
                range: DomainRange::new(1.0, 8.0),
            };
            this.core.trajectory.selected_range = Some(selected);
            this.core.trajectory.visible_range = Some(viewport);

            assert_eq!(
                this.finish_session_open_request(&target_key, generation, &project.id),
                SessionOpenCompletion::Current
            );
            let runtime = this
                .reconcile_loaded_runtime(active_project, loaded_target, cx)
                .unwrap();
            this.select_runtime(runtime, cx);

            let saved = &this.view_states[&source_key];
            assert_eq!(saved.trajectory_query, "edited while loading");
            assert!(!saved.trajectory_follow_tail);
            let saved_offset = saved
                .trajectory_offset
                .expect("the loading-time ledger offset should be saved");
            assert_eq!(saved_offset.item_ix, offset.item_ix);
            assert_eq!(saved_offset.offset_in_item, offset.offset_in_item);
            assert_eq!(
                saved.details_tab_history,
                vec![DetailsTab::Raw, DetailsTab::Timing]
            );
            assert_eq!(saved.details_offset, point(px(0.0), px(-42.0)));
            assert_eq!(
                saved.timeline_selection,
                Some(SavedTimelineRange::capture(selected))
            );
            assert_eq!(
                saved.timeline_viewport,
                Some(SavedTimelineRange::capture(viewport))
            );
            assert_eq!(this.core.session.current, target.path);
            assert_eq!(this.core.surface, Surface::Chat);
            assert!(!this.core.layout_input.trajectory_visible);

            this.select_runtime(source, cx);
            assert_eq!(this.core.surface, Surface::Trajectory);
            assert!(this.core.layout_input.trajectory_visible);
            assert_eq!(this.trajectory_query_value, "edited while loading");
        });
    });

    close_test_window(view, cx);
    std::fs::remove_dir_all(root).unwrap();
}

#[gpui_kit::test]
fn failed_cross_project_open_falls_back_to_the_target_project_draft(
    cx: &mut gpui_kit::TestAppContext,
) {
    let root = std::env::temp_dir().join(format!(
        "castle-desktop-cross-project-open-failure-{}-{}",
        std::process::id(),
        now_ms()
    ));
    let workspace_a = root.join("workspace-a");
    let workspace_b = root.join("workspace-b");
    std::fs::create_dir_all(&workspace_a).unwrap();
    std::fs::create_dir_all(&workspace_b).unwrap();
    let (mut project_store, active_project) =
        ProjectStore::load(root.join("state"), Some(workspace_a.clone())).unwrap();
    let project_b_index = project_store.add(workspace_b).unwrap();
    let project_b = project_store.project(project_b_index).unwrap().clone();
    let settings = SettingsStore::load(root.join("settings")).unwrap();
    let model = Model::new("test", "key", "http://localhost", "test-model", 10_000);
    let profile = ProviderModel::new("test-model", "Test Model", 10_000, None);
    let configured = ConfiguredModel::new("test", profile, model.clone());
    let agent = SessionSetup::new(model, "test", Session::memory(), workspace_a);

    cx.update(crate::bootstrap::init_ui);
    let (view, cx) = cx.add_window_view(|window, cx| {
        let app = DesktopApp::new(
            DesktopStartup {
                agent,
                models: vec![configured],
                selected_model: 0,
                project_store,
                active_project,
                settings,
            },
            window,
            cx,
        );
        window.blur(cx);
        app
    });

    let (previous_entity, draft_entity) = cx.update(|_window, app| {
        view.update(app, |this, cx| {
            let previous_entity = this.selected_runtime.entity_id();
            let draft = this
                .select_or_create_project_draft(project_b_index, cx)
                .expect("target project draft must exist");
            let draft_entity = draft.entity_id();
            let missing_id = SessionId::from_raw("missing-cross-project-session");
            let missing_path = project_b
                .sessions_dir
                .join(format!("{missing_id}.session-v2"));
            this.project_sessions
                .get_mut(&project_b.sessions_dir)
                .unwrap()
                .push(SessionInfo {
                    id: missing_id.clone(),
                    project_id: project_b.id.as_str().to_owned(),
                    path: missing_path,
                    title: "Missing session".into(),
                    created_at: 0,
                    updated_at: 0,
                });
            this.project_runtimes
                .get_mut(&project_b.id)
                .unwrap()
                .selected = missing_id.clone();

            let generation = this.begin_runtime_selection_intent();
            let _ = this.transition(Action::ActivateWorkspace {
                index: project_b_index,
                cwd: project_b.path.clone(),
                sessions_dir: project_b.sessions_dir.clone(),
            });
            let key = (
                project_b.id.clone(),
                this.project_sessions[&project_b.sessions_dir]
                    .iter()
                    .find(|session| session.id == missing_id)
                    .unwrap()
                    .path
                    .clone(),
            );
            assert!(this.start_session_open_request(&key, generation));
            assert!(this.selection_pending());
            assert_eq!(this.core.workspace.active_project, project_b_index);
            assert_eq!(this.selected_runtime.entity_id(), previous_entity);
            let pending = this.pending_runtime_selection.clone().unwrap();
            let key = (project_b.id.clone(), pending.path.clone());
            assert_eq!(
                this.finish_session_open_request(&key, pending.generation, &project_b.id,),
                SessionOpenCompletion::Current
            );
            assert!(this.resolve_failed_runtime_selection(
                pending.generation,
                &project_b.id,
                &pending.path,
                project_b_index,
                cx,
            ));
            (previous_entity, draft_entity)
        })
    });
    cx.read_entity(&view, |this, cx| {
        assert!(!this.selection_pending());
        assert_ne!(this.selected_runtime.entity_id(), previous_entity);
        assert_eq!(this.selected_runtime.entity_id(), draft_entity);
        let (selected_project, _) = this
            .runtime_location(&this.selected_runtime)
            .expect("fallback draft must be registered");
        assert_eq!(selected_project, project_b.id);
        assert_eq!(this.core.workspace.active_project, project_b_index);
        assert!(this.core.session.current.as_os_str().is_empty());
        assert!(
            this.selected_runtime
                .read(cx)
                .snapshot()
                .session
                .path
                .as_os_str()
                .is_empty()
        );
    });

    close_test_window(view, cx);
    std::fs::remove_dir_all(root).unwrap();
}

#[gpui_kit::test]
fn reopening_cached_runtime_reloads_external_revision_and_config_drift(
    cx: &mut gpui_kit::TestAppContext,
) {
    let root = std::env::temp_dir().join(format!(
        "castle-desktop-reopen-cache-{}-{}",
        std::process::id(),
        now_ms()
    ));
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let (project_store, active_project) =
        ProjectStore::load(root.join("state"), Some(workspace.clone())).unwrap();
    let project = project_store.project(active_project).unwrap().clone();
    let initial_config = SessionConfig {
        model: SessionModelConfig {
            model_id: Some("test/test-model".into()),
            reasoning_effort: None,
        },
        allow_all_tools: false,
    };
    let session_info = create_v2_session_with_config(
        &project.sessions_dir,
        project.id.as_str(),
        SessionId::from_raw("external-reopen"),
        Some("Stable title"),
        initial_config.clone(),
    );
    let settings = SettingsStore::load(root.join("settings")).unwrap();
    let model = Model::new("test", "key", "http://localhost", "test-model", 10_000);
    let profile = ProviderModel::new("test-model", "Test Model", 10_000, None);
    let configured = ConfiguredModel::new("test", profile, model.clone());
    let agent = SessionSetup::new(model, "test", Session::memory(), &workspace);

    cx.update(crate::bootstrap::init_ui);
    let (view, cx) = cx.add_window_view(|window, cx| {
        let app = DesktopApp::new(
            DesktopStartup {
                agent,
                models: vec![configured],
                selected_model: 0,
                project_store,
                active_project,
                settings,
            },
            window,
            cx,
        );
        window.blur(cx);
        app
    });

    let stale_entity = cx.update(|_, app| {
        view.update(app, |this, cx| {
            let session =
                Session::open_writable_in_project(&session_info.path, project.id.as_str()).unwrap();
            let runtime = this.create_runtime(active_project, session, cx).unwrap();
            assert_eq!(runtime.read(cx).observation().durable_revision, 0);
            assert!(
                runtime
                    .read(cx)
                    .snapshot()
                    .view
                    .conversation
                    .messages
                    .is_empty()
            );
            runtime.entity_id()
        })
    });

    // Hold the first loader's snapshot at a deterministic gate. A later request for the same
    // key must not promote this snapshot after the store advances.
    let stale_loaded =
        Session::open_writable_in_project(&session_info.path, project.id.as_str()).unwrap();
    assert_eq!(stale_loaded.revision(), 0);
    let open_key = (project.id.clone(), session_info.path.clone());
    let first_generation = cx.update(|_, app| {
        view.update(app, |this, _| {
            let generation = this.begin_runtime_selection_intent();
            this.begin_pending_runtime_selection(
                generation,
                project.id.clone(),
                session_info.path.clone(),
            );
            this.inflight_session_opens
                .insert(open_key.clone(), generation);
            generation
        })
    });

    let external_revision = commit_external_turn(
        &session_info.path,
        project.id.as_str(),
        "external input",
        "external output",
    );
    assert!(external_revision > 0);
    let reload_generation = cx.update(|_, app| {
        view.update(app, |this, _| {
            let generation = this.begin_runtime_selection_intent();
            this.begin_pending_runtime_selection(
                generation,
                project.id.clone(),
                session_info.path.clone(),
            );
            *this.inflight_session_opens.get_mut(&open_key).unwrap() = generation;
            assert_eq!(
                this.finish_session_open_request(&open_key, first_generation, &project.id,),
                SessionOpenCompletion::Reload(generation),
                "the newer same-key intent must force a new SQLite snapshot"
            );
            assert!(!this.inflight_session_opens.contains_key(&open_key));
            assert_eq!(
                this.pending_runtime_selection,
                Some(PendingRuntimeSelection {
                    generation,
                    project_id: project.id.clone(),
                    path: session_info.path.clone(),
                }),
                "the superseding selection gate must survive until the fresh load completes"
            );
            generation
        })
    });
    drop(stale_loaded);

    let loaded_after_commit =
        Session::open_writable_in_project(&session_info.path, project.id.as_str()).unwrap();
    assert_eq!(loaded_after_commit.revision(), external_revision);
    let reloaded_entity = cx.update(|_, app| {
        view.update(app, |this, cx| {
            this.inflight_session_opens
                .insert(open_key.clone(), reload_generation);
            assert_eq!(
                this.finish_session_open_request(&open_key, reload_generation, &project.id,),
                SessionOpenCompletion::Current
            );
            let runtime = this
                .reconcile_loaded_runtime(active_project, loaded_after_commit, cx)
                .unwrap();
            assert!(
                runtime
                    .read(cx)
                    .snapshot()
                    .view
                    .conversation
                    .messages
                    .iter()
                    .any(|message| message.text.contains("external input"))
            );
            this.select_runtime(runtime.clone(), cx);
            assert!(!this.selection_pending());
            runtime.entity_id()
        })
    });
    assert_ne!(reloaded_entity, stale_entity);

    let changed_config = SessionConfig {
        allow_all_tools: true,
        ..initial_config
    };
    persist_external_config(
        &session_info.path,
        project.id.as_str(),
        changed_config.clone(),
    );
    let loaded_after_config =
        Session::open_writable_in_project(&session_info.path, project.id.as_str()).unwrap();
    assert_eq!(loaded_after_config.revision(), external_revision);
    assert_eq!(loaded_after_config.config(), &changed_config);
    let config_reloaded_entity = cx.update(|_, app| {
        view.update(app, |this, cx| {
            let runtime = this
                .reconcile_loaded_runtime(active_project, loaded_after_config, cx)
                .unwrap();
            assert_eq!(runtime.read(cx).snapshot().config, changed_config);
            this.select_runtime(runtime.clone(), cx);
            runtime.entity_id()
        })
    });
    assert_ne!(config_reloaded_entity, reloaded_entity);

    close_test_window(view, cx);
    std::fs::remove_dir_all(root).unwrap();
}

#[gpui_kit::test]
fn background_failed_runtimes_share_the_terminal_cache_bound(cx: &mut gpui_kit::TestAppContext) {
    let root = std::env::temp_dir().join(format!(
        "castle-desktop-settled-runtime-cache-{}-{}",
        std::process::id(),
        now_ms()
    ));
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let (project_store, active_project) =
        ProjectStore::load(root.join("state"), Some(workspace.clone())).unwrap();
    let project = project_store.project(active_project).unwrap().clone();
    let sessions = (0..MAX_CACHED_TERMINAL_RUNTIMES + 2)
        .map(|index| {
            create_v2_session(
                &project.sessions_dir,
                project.id.as_str(),
                SessionId::from_raw(format!("settled-cache-{index}")),
                None,
            )
        })
        .collect::<Vec<_>>();
    let settings = SettingsStore::load(root.join("settings")).unwrap();
    let model = Model::new("test", "key", "http://localhost", "test-model", 10_000);
    let profile = ProviderModel::new("test-model", "Test Model", 10_000, None);
    let configured = ConfiguredModel::new("test", profile, model.clone());
    let agent = SessionSetup::new(model, "test", Session::memory(), workspace);

    cx.update(crate::bootstrap::init_ui);
    let (view, cx) = cx.add_window_view(|window, cx| {
        let app = DesktopApp::new(
            DesktopStartup {
                agent,
                models: vec![configured],
                selected_model: 0,
                project_store,
                active_project,
                settings,
            },
            window,
            cx,
        );
        window.blur(cx);
        app
    });

    let mut evicted_entities = Vec::new();
    cx.update(|_, app| {
        view.update(app, |this, cx| {
            let draft_id = this.selected_runtime.read(cx).observation().session.id;
            let mut runtimes = Vec::new();
            for session_info in &sessions {
                let session =
                    Session::open_writable_in_project(&session_info.path, project.id.as_str())
                        .unwrap();
                let runtime = this.create_runtime(active_project, session, cx).unwrap();
                runtime.update(cx, |runtime, _| {
                    runtime.mark_failed_for_test(format!("failed-{}", session_info.id.as_str()));
                });
                runtimes.push((session_info.id.clone(), runtime));
            }

            // Recreate the app-layer observation immediately before many background runtimes
            // become terminal failures. Prior observations deliberately record their last
            // visible state as active so one synchronization exercises the transition-triggered
            // eviction path, not just registration-time eviction.
            for (index, (session_id, runtime)) in runtimes.iter().enumerate() {
                let key = (project.id.clone(), session_id.clone());
                let subscription = cx.observe(runtime, |this, runtime, cx| {
                    this.sync_runtime_snapshot(&runtime, cx);
                });
                this.project_runtimes
                    .get_mut(&project.id)
                    .unwrap()
                    .sessions
                    .insert(session_id.clone(), runtime.clone());
                this.runtime_subscriptions.insert(key.clone(), subscription);
                this.runtime_recency.insert(key.clone(), index as u64 + 1);
                let observation = runtime.read(cx).observation();
                this.runtime_observations.insert(
                    key,
                    RuntimeObservation {
                        completed_runs: observation.completed_runs,
                        transcript_updates: observation.transcript_updates,
                        catalog_synced_revision: observation.durable_revision,
                        metadata_generation: observation.metadata_generation,
                        is_terminal: false,
                    },
                );
            }
            // A remembered per-project selection is an identity hint, not a cache pin. Make
            // the oldest full runtime the remembered selection and prove it is still evicted.
            this.project_runtimes.get_mut(&project.id).unwrap().selected = runtimes[0].0.clone();

            assert_eq!(
                runtimes
                    .iter()
                    .filter(|(session_id, _)| this.project_runtimes[&project.id]
                        .sessions
                        .contains_key(session_id))
                    .count(),
                MAX_CACHED_TERMINAL_RUNTIMES + 2
            );
            evicted_entities.extend(
                runtimes
                    .iter()
                    .take(2)
                    .map(|(_, runtime)| runtime.downgrade()),
            );
            this.sync_runtime_snapshot(&runtimes.last().unwrap().1, cx);

            let retained = runtimes
                .iter()
                .filter(|(session_id, _)| {
                    this.project_runtimes[&project.id]
                        .sessions
                        .contains_key(session_id)
                })
                .count();
            assert_eq!(retained, MAX_CACHED_TERMINAL_RUNTIMES);
            for (session_id, _) in runtimes.iter().take(2) {
                let key = (project.id.clone(), session_id.clone());
                assert!(
                    !this.project_runtimes[&project.id]
                        .sessions
                        .contains_key(session_id)
                );
                assert!(!this.runtime_subscriptions.contains_key(&key));
                assert!(!this.runtime_recency.contains_key(&key));
                assert!(!this.runtime_observations.contains_key(&key));
            }
            assert_ne!(
                this.project_runtimes[&project.id].selected, draft_id,
                "the test must exercise a remembered persisted selection"
            );
            drop(runtimes);
        });
    });
    cx.run_until_parked();
    assert!(
        evicted_entities
            .iter()
            .all(|runtime| runtime.upgrade().is_none()),
        "eviction must release the runtime entity and its owned document/view"
    );

    close_test_window(view, cx);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn message_render_keys_do_not_alias_across_session_reloads() {
    let first = message(Role::Assistant, "first".into());
    let second = message(Role::Assistant, "second".into());
    assert_ne!(first.key, second.key);
}

#[test]
fn trajectory_folds_are_isolated_by_session_view_state() {
    let assistant = TrajectoryItemId::Assistant(harness::RequestId::from("request-a"));
    let mut first = SessionViewState {
        trajectory_offset: Some(ListOffset {
            item_ix: 17,
            offset_in_item: px(6.0),
        }),
        trajectory_follow_tail: false,
        trajectory_query: "alpha crane".into(),
        ..SessionViewState::default()
    };
    first.collapsed_turns.insert(1);
    first.collapsed_assistants.insert(assistant.clone());
    let second = SessionViewState::default();

    assert_eq!(first.trajectory_offset.unwrap().item_ix, 17);
    assert_eq!(first.trajectory_offset.unwrap().offset_in_item, px(6.0));
    assert_eq!(first.trajectory_query, "alpha crane");
    assert!(!first.trajectory_follow_tail);
    assert!(second.trajectory_offset.is_none());
    assert_eq!(second.trajectory_query, "");
    assert!(second.trajectory_follow_tail);
    assert_eq!(first.collapsed_turns, HashSet::from([1]));
    assert_eq!(first.collapsed_assistants, HashSet::from([assistant]));
    assert!(second.collapsed_turns.is_empty());
    assert!(second.collapsed_assistants.is_empty());
}

#[test]
fn saved_timeline_ranges_rebase_to_a_replayed_session_projection() {
    let original = AxisRange {
        axis: AxisId {
            document_generation: 17,
            geometry_revision: 4,
            mode: TimelineMode::Duration,
        },
        range: DomainRange::new(125.0, 875.0),
    };
    let saved = SavedTimelineRange::capture(original);

    let restored = saved
        .restore(TimelineMode::Duration, 91, 12)
        .expect("the same mode should restore");
    assert_eq!(restored.range, original.range);
    assert_eq!(restored.axis.document_generation, 91);
    assert_eq!(restored.axis.geometry_revision, 12);
    assert_eq!(restored.axis.mode, TimelineMode::Duration);
    assert_ne!(restored.axis, original.axis);
    assert!(saved.restore(TimelineMode::Sequence, 91, 12).is_none());
}

#[gpui_kit::test]
fn evicted_runtime_restores_surface_and_ranges_on_the_fresh_projection_lineage(
    cx: &mut gpui_kit::TestAppContext,
) {
    let root = std::env::temp_dir().join(format!(
        "castle-desktop-evicted-timeline-state-{}-{}",
        std::process::id(),
        now_ms()
    ));
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let (project_store, active_project) =
        ProjectStore::load(root.join("state"), Some(workspace.clone())).unwrap();
    let project = project_store.project(active_project).unwrap().clone();
    let session_info = create_v2_session(
        &project.sessions_dir,
        project.id.as_str(),
        SessionId::from_raw("evicted-timeline-state"),
        Some("Evicted timeline state"),
    );
    let settings = SettingsStore::load(root.join("settings")).unwrap();
    let model = Model::new("test", "key", "http://localhost", "test-model", 10_000);
    let profile = ProviderModel::new("test-model", "Test Model", 10_000, None);
    let configured = ConfiguredModel::new("test", profile, model.clone());
    let agent = SessionSetup::new(model, "test", Session::memory(), workspace);

    cx.update(crate::bootstrap::init_ui);
    let (view, cx) = cx.add_window_view(|window, cx| {
        let app = DesktopApp::new(
            DesktopStartup {
                agent,
                models: vec![configured],
                selected_model: 0,
                project_store,
                active_project,
                settings,
            },
            window,
            cx,
        );
        window.blur(cx);
        app
    });

    let first_session =
        Session::open_writable_in_project(&session_info.path, project.id.as_str()).unwrap();
    let (old_lineage, evicted_runtime) = cx.update(|window, app| {
        view.update(app, |this, cx| {
            let draft = this.selected_runtime.clone();
            let runtime = this
                .create_runtime(active_project, first_session, cx)
                .unwrap();
            let key = (project.id.clone(), session_info.id.clone());
            this.select_runtime(runtime.clone(), cx);
            this.set_trajectory(true, window, cx);
            assert_eq!(this.core.surface, Surface::Trajectory);
            this.select_runtime(draft.clone(), cx);
            assert_eq!(this.core.surface, Surface::Chat);
            this.select_runtime(runtime.clone(), cx);
            assert_eq!(this.core.surface, Surface::Trajectory);

            this.set_trajectory(false, window, cx);
            assert_eq!(this.core.surface, Surface::Chat);
            this.select_runtime(draft.clone(), cx);
            this.select_runtime(runtime.clone(), cx);
            assert_eq!(this.core.surface, Surface::Chat);
            this.set_trajectory(true, window, cx);
            this.select_runtime(runtime.clone(), cx);
            assert_eq!(this.core.surface, Surface::Trajectory);

            let projection = &this.core.session_view.trajectory;
            let old_lineage = projection.projection_lineage();
            let axis = AxisId {
                document_generation: old_lineage,
                geometry_revision: projection.revision(),
                mode: this.core.trajectory.mode,
            };
            this.core.trajectory.selected_range = Some(AxisRange {
                axis,
                range: DomainRange::new(2.0, 4.0),
            });
            this.core.trajectory.visible_range = Some(AxisRange {
                axis,
                range: DomainRange::new(1.0, 8.0),
            });

            // Switching away captures the semantic ranges. Removing the terminal runtime then
            // drops the document and its projection identity exactly like LRU eviction.
            this.select_runtime(draft, cx);
            assert_eq!(this.core.surface, Surface::Chat);
            this.remove_cached_runtime(&key);
            (old_lineage, runtime.downgrade())
        })
    });
    cx.run_until_parked();
    assert!(
        evicted_runtime.upgrade().is_none(),
        "the test must drop the original document projection"
    );

    let replayed_session =
        Session::open_writable_in_project(&session_info.path, project.id.as_str()).unwrap();
    cx.update(|_, app| {
        view.update(app, |this, cx| {
            let runtime = this
                .create_runtime(active_project, replayed_session, cx)
                .unwrap();
            let new_lineage = runtime
                .read(cx)
                .snapshot()
                .view
                .trajectory
                .projection_lineage();
            assert_ne!(new_lineage, old_lineage);

            this.select_runtime(runtime, cx);
            assert_eq!(this.core.surface, Surface::Trajectory);
            assert!(this.core.layout_input.trajectory_visible);
            let selection = this
                .core
                .trajectory
                .selected_range
                .expect("selection should survive replay");
            let viewport = this
                .core
                .trajectory
                .visible_range
                .expect("viewport should survive replay");
            assert_eq!(selection.axis.document_generation, new_lineage);
            assert_eq!(viewport.axis.document_generation, new_lineage);
            assert_eq!(selection.range, DomainRange::new(2.0, 4.0));
            assert_eq!(viewport.range, DomainRange::new(1.0, 8.0));
        });
    });

    close_test_window(view, cx);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn exported_session_names_are_safe() {
    assert_eq!(safe_file_name("Fix: auth/login?"), "Fix- auth-login");
    assert_eq!(safe_file_name("\u{4f1a}\u{8bdd}"), "\u{4f1a}\u{8bdd}");
}

#[test]
fn sidebar_status_prioritizes_actionable_and_live_states_over_unread() {
    assert_eq!(
        resolve_sidebar_session_status(&SessionConnectionStatus::Running, true, true),
        Some(SidebarSessionStatus::ApprovalNeeded)
    );
    assert_eq!(
        resolve_sidebar_session_status(&SessionConnectionStatus::Running, false, true),
        Some(SidebarSessionStatus::Running)
    );
    assert_eq!(
        resolve_sidebar_session_status(&SessionConnectionStatus::Idle, false, true),
        Some(SidebarSessionStatus::Unread)
    );
    assert_eq!(
        resolve_sidebar_session_status(&SessionConnectionStatus::Idle, false, false),
        None
    );
}

#[test]
fn only_new_background_completions_become_unread() {
    assert!(has_new_unread_completion(0, 1, false));
    assert!(!has_new_unread_completion(0, 1, true));
    assert!(!has_new_unread_completion(1, 1, false));
}

#[test]
fn only_selected_runtime_updates_drive_the_visible_transcript() {
    assert_eq!(visible_transcript_update_count(2, 5, true), 3);
    assert_eq!(visible_transcript_update_count(2, 5, false), 0);
    assert_eq!(visible_transcript_update_count(5, 5, true), 0);
}

#[test]
fn streaming_only_follows_when_the_view_is_near_the_tail() {
    assert!(within_bottom_threshold(px(500.0), px(-480.0)));
    assert!(within_bottom_threshold(px(500.0), px(-500.0)));
    assert!(!within_bottom_threshold(px(500.0), px(-450.0)));
}

#[test]
fn repeated_scroll_events_do_not_request_redundant_chat_renders() {
    let mut follow = true;
    let mut unread = 0;

    assert!(update_chat_follow_on_scroll(
        px(1.0),
        false,
        &mut follow,
        &mut unread,
    ));
    assert!(!follow);

    assert!(!update_chat_follow_on_scroll(
        px(1.0),
        false,
        &mut follow,
        &mut unread,
    ));
    assert!(!follow);
    assert_eq!(unread, 0);

    unread = 3;
    assert!(update_chat_follow_on_scroll(
        px(-1.0),
        true,
        &mut follow,
        &mut unread,
    ));
    assert!(follow);
    assert_eq!(unread, 0);

    assert!(!update_chat_follow_on_scroll(
        px(-1.0),
        true,
        &mut follow,
        &mut unread,
    ));
}

#[test]
fn search_snippets_are_compact_and_unicode_safe() {
    assert_eq!(truncate_chars("  hello   world  ", 20), "hello world");
    assert_eq!(truncate_chars("中文会话内容", 4), "中文会话…");
    assert_eq!(
        matching_search_snippet(
            &["session".into(), "The requested ambiguous phrase".into()],
            "ambiguous"
        ),
        Some("The requested ambiguous phrase".into())
    );
}
