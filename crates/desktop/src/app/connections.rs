use super::*;

impl DesktopApp {
    pub(super) fn sync_runtime_snapshot(
        &mut self,
        runtime: &Entity<SessionConnection>,
        cx: &mut Context<Self>,
    ) {
        let observation = runtime.read(cx).observation();
        let location = self.runtime_location(runtime);
        let selected = runtime.entity_id() == self.selected_runtime.entity_id();
        let mut selected_transcript_updates = 0;
        let mut became_terminal = false;
        if let Some((project_id, session_id)) = &location {
            let key = (project_id.clone(), session_id.clone());
            let previous_observation = self
                .runtime_observations
                .get(&key)
                .copied()
                .unwrap_or_default();
            let unread_completion = has_new_unread_completion(
                previous_observation.completed_runs,
                observation.completed_runs,
                selected,
            );
            selected_transcript_updates = visible_transcript_update_count(
                previous_observation.transcript_updates,
                observation.transcript_updates,
                selected,
            );
            if unread_completion {
                self.unread_sessions.insert(key.clone());
            }
            if selected {
                self.unread_sessions.remove(&key);
            }

            let catalog_missing = !observation.session.path.as_os_str().is_empty()
                && !self
                    .project_store
                    .projects()
                    .iter()
                    .find(|project| &project.id == project_id)
                    .and_then(|project| self.project_sessions.get(&project.sessions_dir))
                    .is_some_and(|sessions| {
                        sessions
                            .iter()
                            .any(|info| info.id == observation.session.id)
                    });
            self.upsert_runtime_session_metadata(project_id, &observation.session);
            let catalog_boundary = matches!(
                observation.status,
                SessionConnectionStatus::Idle
                    | SessionConnectionStatus::Settling
                    | SessionConnectionStatus::Failed(_)
            );
            let should_refresh_catalog = !observation.session.path.as_os_str().is_empty()
                && (catalog_missing
                    || observation.metadata_generation != previous_observation.metadata_generation
                    || (catalog_boundary
                        && observation.durable_revision
                            != previous_observation.catalog_synced_revision));
            let catalog_refreshed =
                should_refresh_catalog && self.refresh_project_catalog(project_id);
            let is_terminal = !runtime.read(cx).is_active();
            became_terminal = !previous_observation.is_terminal && is_terminal;
            self.runtime_observations.insert(
                key,
                RuntimeObservation {
                    completed_runs: observation.completed_runs,
                    transcript_updates: observation.transcript_updates,
                    catalog_synced_revision: if catalog_refreshed {
                        observation.durable_revision
                    } else {
                        previous_observation.catalog_synced_revision
                    },
                    metadata_generation: observation.metadata_generation,
                    is_terminal,
                },
            );
        }
        if location.is_none() && !observation.session.path.as_os_str().is_empty() {
            let project = self
                .project_store
                .projects()
                .iter()
                .find(|project| project.id.as_str() == observation.session.project_id)
                .cloned();
            if let Some(project) = project
                && !self
                    .project_sessions
                    .get(&project.sessions_dir)
                    .is_some_and(|sessions| {
                        sessions
                            .iter()
                            .any(|info| info.id == observation.session.id)
                    })
            {
                self.refresh_project_catalog(&project.id);
            }
        }
        if became_terminal {
            self.evict_terminal_runtimes(cx);
        }
        if !selected {
            cx.notify();
            return;
        }
        let snapshot = runtime.read(cx).snapshot();
        self.apply_selected_runtime_snapshot(snapshot);
        if selected_transcript_updates > 0 {
            self.dispatch_local(
                Action::StreamDeltasReceived(selected_transcript_updates),
                cx,
            );
        }
        cx.notify();
    }

    pub(super) fn runtime_location(
        &self,
        runtime: &Entity<SessionConnection>,
    ) -> Option<(ProjectId, SessionId)> {
        self.project_runtimes
            .iter()
            .find_map(|(project_id, runtimes)| {
                runtimes
                    .sessions
                    .iter()
                    .find(|(_, candidate)| candidate.entity_id() == runtime.entity_id())
                    .map(|(session_id, _)| (project_id.clone(), session_id.clone()))
            })
    }

    pub(super) fn apply_selected_runtime_snapshot(&mut self, snapshot: SessionConnectionSnapshot) {
        if let Some(model_id) = snapshot.config.model.model_id.as_deref()
            && let Some(index) = self.models.iter().position(|model| model.id == model_id)
            && self.models[index].model.has_api_key()
        {
            self.selected_model = index;
            self.model = self.models[index].label();
        }
        self.selected_reasoning_effort = snapshot.config.model.reasoning_effort;
        let previous_path = self.core.session.current.clone();
        let session_id = snapshot.session.id.clone();
        self.core.session_view = snapshot.view;
        if previous_path != snapshot.session.path {
            self.core.transient_messages.clear();
        }
        self.core.approval = snapshot.approval;
        self.core.session.current = snapshot.session.path.clone();
        self.selected_started_at = snapshot.started_at;
        self.core.run = match snapshot.status {
            SessionConnectionStatus::Idle => RunState::Idle,
            SessionConnectionStatus::Creating | SessionConnectionStatus::Configuring => {
                RunState::Preparing
            }
            SessionConnectionStatus::Running => RunState::Running {
                run: snapshot.active_run.unwrap_or_default(),
            },
            SessionConnectionStatus::Settling => RunState::Running {
                run: snapshot.active_run.unwrap_or_default(),
            },
            SessionConnectionStatus::Failed(failure) => RunState::Failed { failure },
        };
        if previous_path != snapshot.session.path {
            if let Some(project) = self
                .project_store
                .project(self.core.workspace.active_project)
                && let Some(runtimes) = self.project_runtimes.get_mut(&project.id)
            {
                runtimes.selected = snapshot.session.id.clone();
            }
            let needs_catalog_refresh = !snapshot.session.path.as_os_str().is_empty()
                && !self
                    .project_store
                    .project(self.core.workspace.active_project)
                    .and_then(|project| self.project_sessions.get(&project.sessions_dir))
                    .is_some_and(|sessions| {
                        sessions.iter().any(|session| session.id == session_id)
                    });
            if needs_catalog_refresh
                && let Some(project_id) = self
                    .project_store
                    .project(self.core.workspace.active_project)
                    .map(|project| project.id.clone())
            {
                self.refresh_project_catalog(&project_id);
            }
        }
    }

    pub(super) fn register_runtime(
        &mut self,
        project_id: ProjectId,
        runtime: Entity<SessionConnection>,
        cx: &mut Context<Self>,
    ) {
        let observation = runtime.read(cx).observation();
        let session_id = observation.session.id;
        let key = (project_id.clone(), session_id.clone());
        let subscription = cx.observe(&runtime, |this, runtime, cx| {
            this.sync_runtime_snapshot(&runtime, cx);
        });
        self.runtime_subscriptions.insert(key.clone(), subscription);
        self.runtime_observations
            .entry(key.clone())
            .or_insert(RuntimeObservation {
                completed_runs: observation.completed_runs,
                transcript_updates: observation.transcript_updates,
                catalog_synced_revision: observation.durable_revision,
                metadata_generation: observation.metadata_generation,
                is_terminal: !runtime.read(cx).is_active(),
            });
        let project =
            self.project_runtimes
                .entry(project_id)
                .or_insert_with(|| ProjectSessionRuntimes {
                    selected: session_id.clone(),
                    sessions: HashMap::new(),
                });
        project.sessions.insert(session_id, runtime);
        self.touch_runtime(&key);
        self.evict_terminal_runtimes(cx);
    }

    pub(super) fn touch_runtime(&mut self, key: &RuntimeKey) {
        self.runtime_access_clock = self.runtime_access_clock.saturating_add(1);
        self.runtime_recency
            .insert(key.clone(), self.runtime_access_clock);
    }

    /// Keeps a bounded cache of reopenable, inactive runtime documents. Only the globally selected
    /// runtime, active runtimes, and lightweight drafts are protected. A project's remembered
    /// selection is just an identity hint: if its document is evicted, switching back reloads that
    /// session from the store. Eviction drops the GPUI subscription together with the entity so
    /// reopening has exactly one owner and observer.
    pub(super) fn evict_terminal_runtimes(&mut self, cx: &Context<Self>) {
        let selected_entity = self.selected_runtime.entity_id();
        let mut candidates = Vec::new();
        for (project_id, runtimes) in &self.project_runtimes {
            for (session_id, runtime) in &runtimes.sessions {
                if runtime.entity_id() == selected_entity {
                    continue;
                }
                let observation = runtime.read(cx).observation();
                if observation.session.path.as_os_str().is_empty() || runtime.read(cx).is_active() {
                    continue;
                }
                let key = (project_id.clone(), session_id.clone());
                candidates.push((
                    self.runtime_recency.get(&key).copied().unwrap_or_default(),
                    key,
                ));
            }
        }
        if candidates.len() <= MAX_CACHED_TERMINAL_RUNTIMES {
            return;
        }
        candidates.sort_by_key(|(recency, _)| *recency);
        let remove_count = candidates.len() - MAX_CACHED_TERMINAL_RUNTIMES;
        for (_, key) in candidates.into_iter().take(remove_count) {
            self.remove_cached_runtime(&key);
        }
    }

    pub(super) fn remove_cached_runtime(&mut self, key: &RuntimeKey) {
        if let Some(runtimes) = self.project_runtimes.get_mut(&key.0) {
            runtimes.sessions.remove(&key.1);
        }
        self.runtime_subscriptions.remove(key);
        self.runtime_recency.remove(key);
        self.runtime_observations.remove(key);
    }

    pub(super) fn create_runtime(
        &mut self,
        project_index: usize,
        session: Session,
        cx: &mut Context<Self>,
    ) -> Option<Entity<SessionConnection>> {
        let project = self.project_store.project(project_index)?.clone();
        if let Some(runtime) = self
            .project_runtimes
            .get(&project.id)
            .and_then(|runtimes| runtimes.sessions.get(&session.info().id))
            .cloned()
        {
            return Some(runtime);
        }
        let document = match SessionDocument::from_events(session.events().to_vec()) {
            Ok(document) => document,
            Err(_) => return None,
        };
        let is_draft = session.info().path.as_os_str().is_empty();
        let mut config = session.config().clone();
        let preferred_model = if is_draft {
            Some(self.models[self.selected_model].id.as_str())
        } else {
            config.model.model_id.as_deref()
        };
        let model_index =
            active_model_index(&self.models, preferred_model).unwrap_or(self.selected_model);
        let configured = &self.models[model_index];
        if is_draft {
            config = config_for_model(configured, self.settings.allow_all_tools());
        }
        let needs_model_selection = config.model.model_id.as_deref() != Some(&configured.id);
        let agent = SessionSetup::new(
            configured.model.clone(),
            harness::config::INSTRUCTIONS,
            session,
            project.path.clone(),
        );
        let runtime = cx.new(|cx| {
            SessionConnection::new(
                &cx.global::<crate::session::ApplicationHarness>().0,
                agent,
                project.id.as_str().to_owned(),
                project.sessions_dir,
                document,
                config,
            )
        });
        if needs_model_selection {
            runtime.update(cx, |runtime, cx| runtime.select_model(configured, cx));
        }
        self.register_runtime(project.id, runtime.clone(), cx);
        Some(runtime)
    }

    /// Reuses a cached runtime only when its idle SessionSetup, metadata, configuration, and journal
    /// revision all match the snapshot just loaded from SQLite. An active runtime remains the
    /// single owner if an asynchronous open races with a new run.
    pub(super) fn reconcile_loaded_runtime(
        &mut self,
        project_index: usize,
        session: Session,
        cx: &mut Context<Self>,
    ) -> Option<Entity<SessionConnection>> {
        let project = self.project_store.project(project_index)?.clone();
        let key = (project.id.clone(), session.info().id.clone());
        if let Some(runtime) = self
            .project_runtimes
            .get(&project.id)
            .and_then(|runtimes| runtimes.sessions.get(&session.info().id))
            .cloned()
        {
            if runtime.read(cx).is_active() || runtime.read(cx).matches_loaded_session(&session) {
                return Some(runtime);
            }
            self.remove_cached_runtime(&key);
        }
        self.create_runtime(project_index, session, cx)
    }

    pub(super) fn active_project_runtime(&self, path: &Path) -> Option<Entity<SessionConnection>> {
        self.project_runtime(self.core.workspace.active_project, path)
    }

    pub(super) fn project_runtime(
        &self,
        project_index: usize,
        path: &Path,
    ) -> Option<Entity<SessionConnection>> {
        let project = self.project_store.project(project_index)?;
        let session_id = self
            .project_sessions
            .get(&project.sessions_dir)?
            .iter()
            .find(|session| same_path(&session.path, path))?
            .id
            .clone();
        self.project_runtimes
            .get(&project.id)?
            .sessions
            .get(&session_id)
            .cloned()
    }

    pub(crate) fn session_is_active(
        &self,
        project_index: usize,
        path: &Path,
        cx: &Context<Self>,
    ) -> bool {
        self.project_runtime(project_index, path)
            .is_some_and(|runtime| runtime.read(cx).is_active())
    }

    pub(crate) fn session_status_indicator(
        &self,
        project_index: usize,
        path: &Path,
        cx: &Context<Self>,
    ) -> Option<SidebarSessionStatus> {
        let project_id = self.project_store.project(project_index)?.id.clone();
        let observation = self
            .project_runtime(project_index, path)?
            .read(cx)
            .observation();
        let unread = self
            .unread_sessions
            .contains(&(project_id, observation.session.id));
        resolve_sidebar_session_status(&observation.status, observation.approval_needed, unread)
    }

    pub(crate) fn project_has_active_sessions(
        &self,
        project_index: usize,
        cx: &Context<Self>,
    ) -> bool {
        let Some(project) = self.project_store.project(project_index) else {
            return false;
        };
        self.project_runtimes
            .get(&project.id)
            .is_some_and(|runtimes| {
                runtimes
                    .sessions
                    .values()
                    .any(|runtime| runtime.read(cx).is_active())
            })
    }

    #[cfg(not(test))]
    pub(crate) fn has_active_sessions(&self, cx: &Context<Self>) -> bool {
        self.project_runtimes.values().any(|runtimes| {
            runtimes
                .sessions
                .values()
                .any(|runtime| runtime.read(cx).is_active())
        })
    }

    pub(super) fn target_session_info(
        &self,
        project_index: usize,
        path: &Path,
    ) -> Option<SessionInfo> {
        let project = self.project_store.project(project_index)?;
        self.project_sessions
            .get(&project.sessions_dir)?
            .iter()
            .find(|session| same_path(&session.path, path))
            .cloned()
    }

    pub(super) fn select_runtime(
        &mut self,
        runtime: Entity<SessionConnection>,
        cx: &mut Context<Self>,
    ) {
        let changing_session = runtime.entity_id() != self.selected_runtime.entity_id();
        if changing_session {
            self.relations_hovered = false;
            self.relations_open = false;
            let current = self.selected_runtime.read(cx).snapshot().session.id;
            let current_text = self.input.read(cx).value().to_string();
            let text = self
                .composer_restore
                .take()
                .filter(|(_, displaced)| *displaced == current_text)
                .map_or_else(|| current_text.clone(), |(text, _)| text);
            self.composer_drafts
                .insert(current, (text, self.edit_draft.take()));
            let (text, edit) = self
                .composer_drafts
                .remove(&runtime.read(cx).snapshot().session.id)
                .unwrap_or_default();
            self.composer_restore = Some((text, current_text));
            self.edit_draft = edit;
            // A session load is asynchronous, so the user may keep changing search, tabs, tail
            // following, or scroll positions after the click that started it. Capture once more
            // at the atomic handoff; the earlier eager capture remains useful for a failed load,
            // while this one guarantees a successful load cannot drop those later edits.
            self.save_current_view_state();
        }
        let location = self.runtime_location(&runtime);
        if let Some(key) = &location {
            self.unread_sessions.remove(key);
        }
        // Publishing the target entity and retiring the selection capability happen in one GPUI
        // update. No command can observe the new selection while an old loading gate remains.
        self.selected_runtime = runtime.clone();
        self.pending_runtime_selection = None;
        self.timeline_drag = None;
        self.timeline_hover = None;
        self.request_marker_hover = None;
        let snapshot = runtime.read(cx).snapshot();
        if let Some((project_id, _)) = &location
            && let Some(runtimes) = self.project_runtimes.get_mut(project_id)
        {
            runtimes.selected = snapshot.session.id.clone();
        }
        self.apply_selected_runtime_snapshot(snapshot);
        self.sync_message_presentations();
        if let Some(key) = &location {
            let observation = runtime.read(cx).observation();
            let previous = self.runtime_observations.entry(key.clone()).or_default();
            previous.completed_runs = observation.completed_runs;
            previous.transcript_updates = observation.transcript_updates;
            previous.metadata_generation = observation.metadata_generation;
            self.touch_runtime(key);
        }
        if changing_session {
            let surface = location
                .as_ref()
                .and_then(|key| self.view_states.get(key))
                .map(|state| state.surface)
                .unwrap_or_default();
            let _ = self.transition(match surface {
                Surface::Chat => Action::ShowChat,
                Surface::Trajectory => Action::ShowTrajectory,
            });
        }
        self.restore_current_view_state(cx);
        self.evict_terminal_runtimes(cx);
        cx.notify();
    }

    /// Advances the single user-selection epoch used by asynchronous session opens.
    ///
    /// Every synchronous user choice (including choosing the current draft again) must advance
    /// this epoch before it can return. An older open may still finish and populate the bounded
    /// runtime cache, but it can no longer replace the user's newer selection.
    pub(super) fn begin_runtime_selection_intent(&mut self) -> u64 {
        self.open_generation = self.open_generation.saturating_add(1);
        // A newer synchronous choice cancels the preceding loading capability. Async choices set
        // their replacement target before returning to the event loop.
        self.pending_runtime_selection = None;
        self.open_generation
    }

    pub(super) fn begin_pending_runtime_selection(
        &mut self,
        generation: u64,
        project_id: ProjectId,
        path: PathBuf,
    ) {
        self.pending_runtime_selection = Some(PendingRuntimeSelection {
            generation,
            project_id,
            path,
        });
    }

    pub(crate) fn selection_pending(&self) -> bool {
        self.pending_runtime_selection.is_some()
    }

    pub(super) fn pending_selection_targets(&self, project_index: usize, path: &Path) -> bool {
        let Some(project_id) = self
            .project_store
            .project(project_index)
            .map(|project| &project.id)
        else {
            return false;
        };
        self.pending_runtime_selection
            .as_ref()
            .is_some_and(|pending| {
                &pending.project_id == project_id && same_path(&pending.path, path)
            })
    }

    pub(super) fn session_open_matches_current_intent(
        &self,
        requested_generation: u64,
        project_id: &ProjectId,
    ) -> bool {
        requested_generation == self.open_generation
            && self
                .project_store
                .project(self.core.workspace.active_project)
                .is_some_and(|project| &project.id == project_id)
    }

    /// Retires exactly one loader for `key` and decides how its snapshot may be used.
    ///
    /// A later request for the same key takes over the in-flight slot by advancing the requested
    /// generation. The loader that was already running cannot satisfy that later request: its
    /// SQLite snapshot may predate the later click. When that later request is still the current
    /// selection intent, start a fresh load; otherwise discard the superseded result rather than
    /// warming the cache with a snapshot that a newer same-key request explicitly replaced.
    pub(super) fn finish_session_open_request(
        &mut self,
        key: &OpenSessionKey,
        started_generation: u64,
        project_id: &ProjectId,
    ) -> SessionOpenCompletion {
        let Some(requested_generation) = self.inflight_session_opens.remove(key) else {
            return SessionOpenCompletion::Ignore;
        };
        let is_current = self.session_open_matches_current_intent(requested_generation, project_id);
        if requested_generation != started_generation {
            return if is_current {
                SessionOpenCompletion::Reload(requested_generation)
            } else {
                SessionOpenCompletion::Ignore
            };
        }
        if is_current {
            SessionOpenCompletion::Current
        } else {
            SessionOpenCompletion::WarmCache
        }
    }

    /// Installs the selection capability before a loader can run. Returning `false` means an
    /// existing same-key loader now owns the newer generation and must be allowed to retire into
    /// `Reload`; no second loader should be spawned yet.
    pub(super) fn start_session_open_request(
        &mut self,
        key: &OpenSessionKey,
        generation: u64,
    ) -> bool {
        if !self.session_open_matches_current_intent(generation, &key.0) {
            return false;
        }
        self.begin_pending_runtime_selection(generation, key.0.clone(), key.1.clone());
        if let Some(requested_generation) = self.inflight_session_opens.get_mut(key) {
            *requested_generation = generation;
            return false;
        }
        self.inflight_session_opens.insert(key.clone(), generation);
        true
    }

    /// Resolves a failed current selection without ever exposing another project's runtime under
    /// the target workspace. Same-project failures keep the prior coherent selection; cross-
    /// project failures atomically fall back to the target project's draft.
    pub(super) fn resolve_failed_runtime_selection(
        &mut self,
        generation: u64,
        project_id: &ProjectId,
        path: &Path,
        project_index: usize,
        cx: &mut Context<Self>,
    ) -> bool {
        let matches_pending = self
            .pending_runtime_selection
            .as_ref()
            .is_some_and(|pending| {
                pending.generation == generation
                    && &pending.project_id == project_id
                    && same_path(&pending.path, path)
            });
        if !matches_pending {
            return false;
        }

        let selected_belongs_to_target = self
            .runtime_location(&self.selected_runtime)
            .is_some_and(|(selected_project_id, _)| &selected_project_id == project_id);
        if selected_belongs_to_target {
            self.pending_runtime_selection = None;
            return false;
        }

        let Some(runtime) = self.select_or_create_project_draft(project_index, cx) else {
            // Retain the gate if the target project disappeared unexpectedly. A newer project
            // selection can supersede it, but no command may fall through to the old runtime.
            return false;
        };
        self.select_runtime(runtime, cx);
        true
    }

    pub(super) fn select_or_create_project_draft(
        &mut self,
        project_index: usize,
        cx: &mut Context<Self>,
    ) -> Option<Entity<SessionConnection>> {
        let project = self.project_store.project(project_index)?.clone();
        if let Some(runtime) = self
            .project_runtimes
            .get(&project.id)
            .and_then(|runtimes| {
                runtimes.sessions.values().find(|runtime| {
                    runtime
                        .read(cx)
                        .observation()
                        .session
                        .path
                        .as_os_str()
                        .is_empty()
                })
            })
            .cloned()
        {
            return Some(runtime);
        }
        self.create_runtime(project_index, Session::memory(), cx)
    }

    pub(crate) fn open_session(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let generation = self.begin_runtime_selection_intent();
        if path == self.core.session.current {
            return;
        }
        self.save_current_view_state();
        if let Some(runtime) = self.active_project_runtime(&path) {
            if runtime.read(cx).is_active() {
                self.select_runtime(runtime, cx);
                self.input.update(cx, |input, cx| {
                    input.set_placeholder("Message the agent", window, cx);
                    input.focus(window, cx);
                });
            } else {
                self.open_session_async(path, generation, window, cx);
            }
            return;
        }
        self.open_session_async(path, generation, window, cx);
    }

    pub(super) fn open_session_async(
        &mut self,
        path: PathBuf,
        generation: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let project_index = self.core.workspace.active_project;
        let Some(project) = self.project_store.project(project_index).cloned() else {
            return;
        };
        let project_id = project.id.clone();
        let storage_project_id = project.id.as_str().to_owned();
        let key = (project_id.clone(), path.clone());
        if !self.start_session_open_request(&key, generation) {
            return;
        }
        cx.spawn_in(window, async move |this, cx| {
            let session = Session::open_in_project(path, &storage_project_id).await;
            let _ = cx.update(|window, app| {
                if let Some(this) = this.upgrade() {
                    this.update(app, |this, cx| {
                        let completion =
                            this.finish_session_open_request(&key, generation, &project_id);
                        if let SessionOpenCompletion::Reload(requested_generation) = completion {
                            this.open_session_async(
                                key.1.clone(),
                                requested_generation,
                                window,
                                cx,
                            );
                            return;
                        }
                        if completion == SessionOpenCompletion::Ignore {
                            return;
                        }
                        let resolved_project_index = this
                            .project_store
                            .project(project_index)
                            .filter(|project| project.id == project_id)
                            .map(|_| project_index)
                            .or_else(|| {
                                this.project_store
                                    .projects()
                                    .iter()
                                    .position(|project| project.id == project_id)
                            });
                        let is_current_intent = completion == SessionOpenCompletion::Current;
                        match session {
                            Ok(session) => {
                                if let Some(project_index) = resolved_project_index {
                                    let runtime = if is_current_intent {
                                        this.reconcile_loaded_runtime(project_index, session, cx)
                                    } else {
                                        // A stale completion may warm an empty cache, but it must
                                        // never replace a runtime that the user selected or started
                                        // after this request began.
                                        this.create_runtime(project_index, session, cx)
                                    };
                                    if is_current_intent {
                                        if let Some(runtime) = runtime {
                                            this.select_runtime(runtime, cx);
                                            this.input.update(cx, |input, cx| {
                                                input.set_placeholder(
                                                    "Message the agent",
                                                    window,
                                                    cx,
                                                );
                                                input.focus(window, cx);
                                            });
                                        } else {
                                            this.discard_invalid_session_catalog_entry(
                                                project_index,
                                                &key.1,
                                            );
                                            let fell_back = this.resolve_failed_runtime_selection(
                                                generation,
                                                &project_id,
                                                &key.1,
                                                project_index,
                                                cx,
                                            );
                                            if fell_back {
                                                this.input.update(cx, |input, cx| {
                                                    input.set_placeholder(
                                                        "Describe what you want to build",
                                                        window,
                                                        cx,
                                                    );
                                                });
                                            }
                                            this.input
                                                .update(cx, |input, cx| input.focus(window, cx));
                                        }
                                    } else if runtime.is_none() {
                                        this.discard_invalid_session_catalog_entry(
                                            project_index,
                                            &key.1,
                                        );
                                    }
                                }
                            }
                            Err(error) => {
                                let invalid = invalid_session_open_error(&error);
                                if let Some(project_index) = resolved_project_index {
                                    if invalid {
                                        this.discard_invalid_session_catalog_entry(
                                            project_index,
                                            &key.1,
                                        );
                                    }
                                    if is_current_intent {
                                        let fell_back = this.resolve_failed_runtime_selection(
                                            generation,
                                            &project_id,
                                            &key.1,
                                            project_index,
                                            cx,
                                        );
                                        if fell_back {
                                            this.input.update(cx, |input, cx| {
                                                input.set_placeholder(
                                                    "Describe what you want to build",
                                                    window,
                                                    cx,
                                                );
                                            });
                                        }
                                    }
                                }
                                if is_current_intent {
                                    if let Some(message) = session_open_error_notice(&error) {
                                        this.notice(message);
                                    }
                                    this.input.update(cx, |input, cx| input.focus(window, cx));
                                }
                            }
                        }
                        cx.notify();
                    });
                }
            });
        })
        .detach();
    }

    pub(crate) fn open_project_session(
        &mut self,
        project_index: usize,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if project_index != self.core.workspace.active_project {
            self.switch_project(project_index, window, cx);
        }
        if project_index == self.core.workspace.active_project {
            self.open_session(path, window, cx);
        }
    }

    pub(crate) fn rename_target_session(
        &mut self,
        project_index: usize,
        path: PathBuf,
        title: String,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pending_selection_targets(project_index, &path) {
            self.notice("Wait for this session to finish opening before renaming it");
            return;
        }
        if path.as_os_str().is_empty() || self.session_is_active(project_index, &path, cx) {
            self.notice("This session cannot be renamed while it is active");
            return;
        }
        let runtime = self.project_runtime(project_index, &path).or_else(|| {
            let project_id = self.project_store.project(project_index)?.id.as_str();
            let session = Session::open_writable_in_project(&path, project_id).ok()?;
            self.create_runtime(project_index, session, cx)
        });
        let Some(runtime) = runtime else {
            self.notice("Could not open the target session for renaming");
            return;
        };
        if !runtime.update(cx, |runtime, cx| runtime.rename(title, cx)) {
            self.notice("This session cannot be renamed while it is active");
        }
    }

    pub(crate) fn archive_target_session(
        &mut self,
        project_index: usize,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pending_selection_targets(project_index, &path) {
            self.notice("Wait for this session to finish opening before archiving it");
            return;
        }
        if path.as_os_str().is_empty() || self.session_is_active(project_index, &path, cx) {
            self.notice("This session cannot be archived while it is active");
            return;
        }
        let Some(session) = self.target_session_info(project_index, &path) else {
            self.notice("Could not find the target session");
            return;
        };
        match Session::archive(&session) {
            Ok(_) => {
                self.remove_session_projection(project_index, &session, &path, window, cx);
                self.reload_archived_sessions(project_index);
                self.notice(format!("Archived “{}”", session.title));
            }
            Err(error) => self.notice(format!("Could not archive session: {error}")),
        }
        cx.notify();
    }

    pub(crate) fn restore_archived_session(
        &mut self,
        project_index: usize,
        session: SessionInfo,
        cx: &mut Context<Self>,
    ) -> Option<SessionInfo> {
        let restored = match Session::restore(&session) {
            Ok(restored) => {
                self.reload_archived_sessions(project_index);
                self.reload_project_session_list(project_index);
                self.notice(format!("Restored “{}”", session.title));
                Some(restored)
            }
            Err(error) => {
                self.notice(format!("Could not restore session: {error}"));
                None
            }
        };
        cx.notify();
        restored
    }

    pub(crate) fn delete_archived_session(
        &mut self,
        project_index: usize,
        session: SessionInfo,
        cx: &mut Context<Self>,
    ) {
        match Session::delete(&session) {
            Ok(()) => {
                self.reload_archived_sessions(project_index);
                self.notice(format!("Deleted “{}”", session.title));
            }
            Err(error) => self.notice(format!("Could not delete archived session: {error}")),
        }
        cx.notify();
    }

    pub(super) fn remove_session_projection(
        &mut self,
        project_index: usize,
        session: &SessionInfo,
        previous_path: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(project) = self.project_store.project(project_index) {
            let key = (project.id.clone(), session.id.clone());
            let namespace = presentation_namespace(&project.id, &session.id);
            self.remove_cached_runtime(&key);
            self.message_presentations
                .get_mut()
                .remove_session(&namespace);
            self.unread_sessions.remove(&key);
        }
        self.reload_project_session_list(project_index);
        let removed_selected = project_index == self.core.workspace.active_project
            && same_path(previous_path, &self.core.session.current);
        if removed_selected {
            self.begin_runtime_selection_intent();
            if let Some(runtime) = self.select_or_create_project_draft(project_index, cx) {
                self.select_runtime(runtime, cx);
                self.input.update(cx, |input, cx| {
                    input.set_placeholder("Describe what you want to build", window, cx);
                    input.focus(window, cx);
                });
            }
        }
    }

    pub(super) fn reload_project_session_list(&mut self, project_index: usize) {
        let project_id = self
            .project_store
            .project(project_index)
            .map(|project| project.id.clone());
        if let Some(project_id) = project_id {
            self.refresh_project_catalog(&project_id);
        }
    }

    pub(super) fn discard_invalid_session_catalog_entry(
        &mut self,
        project_index: usize,
        path: &Path,
    ) {
        let Some(project) = self.project_store.project(project_index).cloned() else {
            return;
        };
        if let Some(session_id) = remove_session_catalog_entry(
            &project.id,
            &project.sessions_dir,
            path,
            &mut self.project_sessions,
            &mut self.session_search_documents,
            &mut self.session_catalog_indices,
        ) {
            self.unread_sessions.remove(&(project.id, session_id));
        }
    }

    pub(super) fn reload_archived_sessions(&mut self, project_index: usize) {
        let Some(project) = self.project_store.project(project_index) else {
            return;
        };
        let sessions_dir = project.sessions_dir.clone();
        match Session::archived_catalog_in_project(&sessions_dir, project.id.as_str()) {
            Ok(catalog) => {
                self.project_archived_sessions
                    .insert(sessions_dir.clone(), catalog.sessions);
            }
            Err(error) if should_clear_catalog_after_error(&error) => {
                self.project_archived_sessions
                    .insert(sessions_dir, Vec::new());
            }
            Err(_) => {}
        }
    }

    pub(crate) fn session_document_matches(&self, path: &Path, query: &str) -> bool {
        self.session_search_documents
            .get(path)
            .is_some_and(|document| document.searchable.contains(query))
    }

    pub(crate) fn session_document_summary(&self, path: &Path, query: &str) -> Option<String> {
        let document = self.session_search_documents.get(path)?;
        matching_search_snippet(&document.snippets, query)
            .or_else(|| (!document.summary.is_empty()).then(|| document.summary.clone()))
    }

    pub(crate) fn session_modified_at(&self, session: &SessionInfo) -> u64 {
        session.updated_at
    }

    pub(super) fn upsert_runtime_session_metadata(
        &mut self,
        project_id: &ProjectId,
        session: &SessionInfo,
    ) {
        if session.path.as_os_str().is_empty() || session.is_archived() {
            return;
        }
        let Some(project) = self
            .project_store
            .projects()
            .iter()
            .find(|project| &project.id == project_id)
        else {
            return;
        };
        let sessions = self
            .project_sessions
            .entry(project.sessions_dir.clone())
            .or_default();
        let key = (project_id.clone(), session.id.clone());
        let index = self
            .session_catalog_indices
            .get(&key)
            .copied()
            .filter(|index| {
                sessions
                    .get(*index)
                    .is_some_and(|existing| existing.id == session.id)
            })
            .or_else(|| {
                sessions
                    .iter()
                    .position(|existing| existing.id == session.id)
            });
        let index = if let Some(index) = index {
            sessions[index] = session.clone();
            index
        } else {
            sessions.push(session.clone());
            sessions.len() - 1
        };
        self.session_catalog_indices.insert(key, index);
    }

    /// Refreshes one project's metadata and search projection from one SQLite catalog snapshot.
    /// Invalid/stale rows remain absent because filtering happens in `SessionStore::catalog`.
    pub(super) fn refresh_project_catalog(&mut self, project_id: &ProjectId) -> bool {
        let Some(project) = self
            .project_store
            .projects()
            .iter()
            .find(|project| &project.id == project_id)
            .cloned()
        else {
            return false;
        };
        self.pending_fork_refresh = true;
        apply_project_catalog_result(
            &project.id,
            &project.sessions_dir,
            Session::catalog_in_project(&project.sessions_dir, project.id.as_str()),
            &mut self.project_sessions,
            &mut self.session_search_documents,
            &mut self.session_catalog_indices,
        )
    }
}
