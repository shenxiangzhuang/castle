use super::*;

impl DesktopApp {
    pub(crate) fn toggle_sidebar_options(&mut self, cx: &mut Context<Self>) {
        self.dispatch_local(Action::ToggleSidebarOptions, cx);
    }

    pub(crate) fn toggle_project(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = self
            .project_store
            .project(index)
            .map(|project| project.path.clone())
        else {
            return;
        };
        if index == self.core.workspace.active_project {
            self.dispatch(Action::ToggleProjectExpanded(path), window, cx);
        } else {
            self.dispatch(Action::ExpandProject(path), window, cx);
            self.switch_project(index, window, cx);
        }
    }

    pub(crate) fn export_session_log(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.selection_pending() {
            return;
        }
        if self.core.session.current.as_os_str().is_empty() {
            self.notice("Start the session before exporting its log");
            cx.notify();
            return;
        }
        let source = self.core.session.current.clone();
        let suggested = format!(
            "{}.jsonl",
            safe_file_name(&self.core.session_view.conversation.title)
        );
        let receiver = cx.prompt_for_new_path(&self.core.workspace.cwd, Some(&suggested));
        cx.spawn_in(window, async move |this, cx| {
            let selection = receiver.await;
            let copied = match selection {
                Ok(Ok(Some(destination))) => tokio::task::spawn_blocking(move || {
                    Session::open_readonly(source)?.export_jsonl(destination)
                })
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result.map_err(|error| error.to_string()))
                .map(|()| Some("Session log exported".to_owned())),
                Ok(Ok(None)) => Ok(None),
                Ok(Err(error)) => Err(error.to_string()),
                Err(error) => Err(error.to_string()),
            };
            let _ = cx.update(|_, app| {
                if let Some(this) = this.upgrade() {
                    this.update(app, |this, cx| {
                        match copied {
                            Ok(Some(message)) => this.notice(message),
                            Ok(None) => {}
                            Err(error) => this.notice(format!("Could not export log: {error}")),
                        }
                        cx.notify();
                    });
                }
            });
        })
        .detach();
    }

    pub(super) fn select_new_chat_draft(&mut self, cx: &mut Context<Self>) -> bool {
        self.begin_runtime_selection_intent();
        self.save_current_view_state();
        let Some(runtime) =
            self.select_or_create_project_draft(self.core.workspace.active_project, cx)
        else {
            self.notice("Could not create a session runtime for this project");
            return false;
        };
        self.select_runtime(runtime, cx);
        true
    }

    pub(crate) fn new_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.select_new_chat_draft(cx) {
            return;
        }
        self.input.update(cx, |input, cx| {
            input.set_placeholder("Describe what you want to build", window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    pub(crate) fn new_chat_in_project(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if index != self.core.workspace.active_project {
            self.switch_project(index, window, cx);
        }
        if index == self.core.workspace.active_project {
            self.new_chat(window, cx);
        }
    }

    pub(crate) fn add_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open Project".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let selection = receiver.await;
            let _ =
                cx.update(|window, app| {
                    if let Some(this) = this.upgrade() {
                        this.update(app, |this, cx| {
                            match selection {
                                Ok(Ok(Some(paths))) => {
                                    if let Some(path) = paths.into_iter().next() {
                                        match this.project_store.add(path) {
                                            Ok(index) => {
                                                if let Some(project) =
                                                    this.project_store.project(index).cloned()
                                                {
                                                    this.project_sessions
                                                        .entry(project.sessions_dir.clone())
                                                        .or_default();
                                                    this.project_archived_sessions
                                                        .entry(project.sessions_dir.clone())
                                                        .or_default();
                                                    this.reload_archived_sessions(index);
                                                    if index == this.core.workspace.active_project {
                                                        this.refresh_project_catalog(&project.id);
                                                    } else {
                                                        this.switch_project(index, window, cx);
                                                    }
                                                }
                                            }
                                            Err(error) => this
                                                .notice(format!("Could not add project: {error}")),
                                        }
                                    }
                                }
                                Ok(Err(error)) => {
                                    this.notice(format!("Could not open project picker: {error}"))
                                }
                                Err(error) => this
                                    .notice(format!("Project picker closed unexpectedly: {error}")),
                                Ok(Ok(None)) => {}
                            }
                            cx.notify();
                        });
                    }
                });
        })
        .detach();
    }

    pub(crate) fn relocate_project(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Relocate Project".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let selection = receiver.await;
            let _ = cx.update(|_, app| {
                if let Some(this) = this.upgrade() {
                    this.update(app, |this, cx| {
                        match selection {
                            Ok(Ok(Some(paths))) => {
                                if let Some(path) = paths.into_iter().next()
                                    && let Err(error) = this.project_store.relocate(index, path)
                                {
                                    this.notice(format!("Could not relocate project: {error}"));
                                }
                            }
                            Ok(Err(error)) => {
                                this.notice(format!("Could not open project picker: {error}"));
                            }
                            Err(error) => {
                                this.notice(format!("Project picker closed unexpectedly: {error}"))
                            }
                            Ok(Ok(None)) => {}
                        }
                        cx.notify();
                    });
                }
            });
        })
        .detach();
    }

    pub(crate) fn switch_project(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if index == self.core.workspace.active_project {
            return;
        }
        let Some(project) = self.project_store.project(index).cloned() else {
            return;
        };
        let generation = self.begin_runtime_selection_intent();
        self.save_current_view_state();
        self.dispatch(
            Action::ActivateWorkspace {
                index,
                cwd: project.path.clone(),
                sessions_dir: project.sessions_dir.clone(),
            },
            window,
            cx,
        );
        self.refresh_project_catalog(&project.id);
        let remembered = self
            .project_runtimes
            .get(&project.id)
            .map(|runtimes| runtimes.selected.clone());
        let runtime = remembered.as_ref().and_then(|session_id| {
            self.project_runtimes
                .get(&project.id)
                .and_then(|runtimes| runtimes.sessions.get(session_id))
                .cloned()
        });
        if let Some(runtime) = runtime {
            let observation = runtime.read(cx).observation();
            if runtime.read(cx).is_active() || observation.session.path.as_os_str().is_empty() {
                self.select_runtime(runtime, cx);
            } else {
                self.open_session_async(observation.session.path, generation, window, cx);
            }
        } else if let Some(path) = remembered.and_then(|session_id| {
            self.project_sessions
                .get(&project.sessions_dir)
                .and_then(|sessions| sessions.iter().find(|session| session.id == session_id))
                .map(|session| session.path.clone())
        }) {
            self.open_session_async(path, generation, window, cx);
        } else if let Some(runtime) = self.select_or_create_project_draft(index, cx) {
            self.select_runtime(runtime, cx);
        }
        self.input.update(cx, |input, cx| {
            input.set_placeholder("Describe what you want to build", window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    pub(crate) fn remove_project(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.project_store.project(index).cloned() else {
            return;
        };
        if project.is_default() {
            self.notice("The default project cannot be removed");
            cx.notify();
            return;
        }
        if self
            .project_runtimes
            .get(&project.id)
            .is_some_and(|runtimes| {
                runtimes
                    .sessions
                    .values()
                    .any(|runtime| runtime.read(cx).is_active())
            })
        {
            self.notice("Stop this project's active sessions before removing it");
            cx.notify();
            return;
        }
        if let Err(error) = self.project_store.remove(index) {
            self.notice(format!("Could not remove project: {error}"));
            cx.notify();
            return;
        }
        let runtime_keys = self
            .project_runtimes
            .get(&project.id)
            .into_iter()
            .flat_map(|runtimes| runtimes.sessions.keys())
            .cloned()
            .map(|session_id| (project.id.clone(), session_id))
            .collect::<Vec<_>>();
        let mut presentation_sessions = runtime_keys
            .iter()
            .map(|(_, session_id)| session_id.clone())
            .collect::<HashSet<_>>();
        if let Some(sessions) = self.project_sessions.get(&project.sessions_dir) {
            presentation_sessions.extend(sessions.iter().map(|session| session.id.clone()));
        }
        for key in runtime_keys {
            self.remove_cached_runtime(&key);
        }
        self.project_runtimes.remove(&project.id);
        for session_id in presentation_sessions {
            self.message_presentations
                .get_mut()
                .remove_session(&presentation_namespace(&project.id, &session_id));
        }
        self.inflight_session_opens
            .retain(|(project_id, _), _| project_id != &project.id);
        self.unread_sessions
            .retain(|(project_id, _)| project_id != &project.id);
        remove_project_catalog_members(
            &project.id,
            &project.sessions_dir,
            &self.project_sessions,
            &mut self.session_search_documents,
            &mut self.session_catalog_indices,
        );
        self.project_sessions.remove(&project.sessions_dir);
        self.project_archived_sessions.remove(&project.sessions_dir);
        let next = if index < self.core.workspace.active_project {
            self.core.workspace.active_project - 1
        } else if index == self.core.workspace.active_project {
            index.min(self.project_store.projects().len() - 1)
        } else {
            self.core.workspace.active_project
        };
        self.dispatch(Action::SetActiveProject(usize::MAX), window, cx);
        self.switch_project(next, window, cx);
    }
}
