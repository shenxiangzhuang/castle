//! Window composition and cross-feature navigation.
mod action;
mod composer;
mod connections;
mod dialogs;
mod effects;
pub(crate) mod layout;
pub(crate) mod presentation;
mod reducer;
mod state;
mod view;
mod workspace;
pub(crate) use action::{Action, Effect, ScrollIntent};
pub(crate) use dialogs::Modal;
use effects::run_effects;
use reducer::reduce;
pub(crate) use state::{
    AppState, ApprovalState, ComposerMenu, DetailsSelection, DetailsTab, INITIAL_SESSION_LIMIT,
    RunState, SESSION_PAGE_SIZE, Surface,
};
mod conversation_tree;
mod session_relations;
use conversation_tree::EditDraft;
#[cfg(test)]
use gpui_kit::ScrollWheelEvent;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::{
    AppContext, Bounds, Context, Entity, FocusHandle, ListAlignment, ListOffset, ListState,
    PathPromptOptions, Pixels, Point, ScrollHandle, Subscription, Window, point, px,
};
#[cfg(test)]
use harness::{Model, SessionCatalog, SessionModelConfig, SessionStoreError};
use harness::{Session, SessionConfig, SessionError, SessionId, SessionInfo, SessionSetup};

use crate::platform::NativeTitlebarController;
use crate::platform::updater::AvailableUpdate;
use crate::session::document::SessionDocument;
use crate::trajectory::timeline::{AxisId, AxisRange, DomainRange};
use crate::trajectory::{
    TimelineModelCache, TrajectoryDetailsLayoutState, TrajectoryDetailsMarkdownCache,
};
use crate::workspace::catalog::{
    SessionCatalogCache, SessionSearchDocument, apply_project_catalog_result,
    load_project_archived_sessions, load_session_catalog_cache, matching_search_snippet,
    remove_project_catalog_members, remove_session_catalog_entry, should_clear_catalog_after_error,
};
#[cfg(test)]
use crate::workspace::catalog::{
    clear_project_catalog_cache, load_session_catalog_cache_with, session_search_document,
    truncate_chars,
};
use crate::{app::layout::LayoutInput, chat::ScrollAnchor};
use crate::{
    chat::{ChatViewport, MessagePresentationStore},
    session::{SessionConnection, SessionConnectionSnapshot, SessionConnectionStatus},
};
use crate::{
    session::{
        LayoutGeneration, Message, Role, TrajectoryItemId, TrajectoryRequestKey, next_message_id,
    },
    trajectory::TimelineMode,
};
use harness::config::ConfiguredModel;
#[cfg(test)]
use harness::config::ProviderModel;
use harness::config::{Appearance, SettingsStore};
use harness::project::{ProjectId, ProjectStore};

pub(crate) fn composer_model_indices(
    models: &[ConfiguredModel],
) -> impl Iterator<Item = usize> + '_ {
    models
        .iter()
        .enumerate()
        .filter(|(_, model)| model.model.has_api_key())
        .map(|(index, _)| index)
}

pub(crate) fn active_model_index(
    models: &[ConfiguredModel],
    preferred_id: Option<&str>,
) -> Option<usize> {
    preferred_id
        .and_then(|preferred| {
            models
                .iter()
                .position(|model| model.id == preferred && model.model.has_api_key())
        })
        .or_else(|| composer_model_indices(models).next())
}

#[derive(Clone, Debug)]
struct SessionViewState {
    surface: Surface,
    chat_anchor: ScrollAnchor,
    trajectory_offset: Option<ListOffset>,
    trajectory_follow_tail: bool,
    trajectory_query: String,
    details_offset: Point<Pixels>,
    selected_details: Option<DetailsSelection>,
    details_tab_history: Vec<DetailsTab>,
    collapsed_turns: HashSet<u32>,
    collapsed_assistants: HashSet<TrajectoryItemId>,
    timeline_selection: Option<SavedTimelineRange>,
    timeline_viewport: Option<SavedTimelineRange>,
}

/// A timeline range saved by meaning rather than by one in-memory projection identity.
///
/// `AxisRange` deliberately carries a projection lineage so a stale live range cannot be applied
/// to another document. Session view state outlives the bounded runtime/document cache, though, so
/// persisting that lineage would also reject a legitimate range when the same session is replayed
/// into a fresh projection. Saving the mode plus domain coordinates lets restore issue a new,
/// correctly stamped range for the current canonical projection.
#[derive(Clone, Copy, Debug, PartialEq)]
struct SavedTimelineRange {
    mode: TimelineMode,
    range: DomainRange,
}

impl SavedTimelineRange {
    fn capture(range: AxisRange) -> Self {
        Self {
            mode: range.axis.mode,
            range: range.range,
        }
    }

    fn restore(
        self,
        mode: TimelineMode,
        document_generation: u64,
        geometry_revision: u64,
    ) -> Option<AxisRange> {
        (self.mode == mode).then_some(AxisRange {
            axis: AxisId {
                document_generation,
                geometry_revision,
                mode,
            },
            range: self.range,
        })
    }
}

impl Default for SessionViewState {
    fn default() -> Self {
        Self {
            surface: Surface::Chat,
            chat_anchor: ScrollAnchor::Tail,
            trajectory_offset: None,
            trajectory_follow_tail: true,
            trajectory_query: String::new(),
            details_offset: point(px(0.0), px(0.0)),
            selected_details: None,
            details_tab_history: vec![DetailsTab::Summary],
            collapsed_turns: HashSet::new(),
            collapsed_assistants: HashSet::new(),
            timeline_selection: None,
            timeline_viewport: None,
        }
    }
}

pub(crate) struct DesktopStartup {
    pub(crate) agent: SessionSetup,
    pub(crate) models: Vec<ConfiguredModel>,
    pub(crate) selected_model: usize,
    pub(crate) project_store: ProjectStore,
    pub(crate) active_project: usize,
    pub(crate) settings: SettingsStore,
}

struct ProjectSessionRuntimes {
    selected: SessionId,
    sessions: HashMap<SessionId, Entity<SessionConnection>>,
}

type RuntimeKey = (ProjectId, SessionId);
type OpenSessionKey = (ProjectId, PathBuf);

const MAX_CACHED_TERMINAL_RUNTIMES: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionOpenCompletion {
    Current,
    WarmCache,
    Reload(u64),
    Ignore,
}

fn invalid_session_open_error(error: &SessionError) -> bool {
    should_clear_catalog_after_error(error)
}

fn session_open_error_notice(error: &SessionError) -> Option<String> {
    (!invalid_session_open_error(error)).then(|| format!("Could not open session: {error}"))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingRuntimeSelection {
    generation: u64,
    project_id: ProjectId,
    path: PathBuf,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct RuntimeObservation {
    completed_runs: u64,
    transcript_updates: u64,
    catalog_synced_revision: u64,
    metadata_generation: u64,
    is_terminal: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct TimelineDragState {
    pub(crate) pan: bool,
    pub(crate) start_value: f64,
    pub(crate) current_value: f64,
    pub(crate) start_x: f32,
    pub(crate) record_id: Option<TrajectoryItemId>,
    pub(crate) initial_viewport: AxisRange,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TimelineHoverState {
    pub(crate) axis: AxisId,
    pub(crate) fraction: f64,
    pub(crate) record_id: Option<TrajectoryItemId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SidebarSessionStatus {
    Preparing,
    Running,
    ApprovalNeeded,
    Failed,
    Unread,
}

pub(crate) struct DesktopApp {
    pub(crate) core: AppState,
    pub(crate) chat: RefCell<ChatViewport>,
    pub(crate) html_previews: crate::chat::html_preview::HtmlPreviews,
    pub(crate) message_presentations: RefCell<MessagePresentationStore>,
    pub(crate) selected_runtime: Entity<SessionConnection>,
    project_runtimes: HashMap<ProjectId, ProjectSessionRuntimes>,
    pub(crate) input: Entity<TextareaState>,
    pub(crate) composer_submitting: bool,
    composer_edit_revision: u64,
    pub(crate) edit_draft: Option<EditDraft>,
    pub(crate) relations_hovered: bool,
    pub(crate) relations_open: bool,
    composer_drafts: HashMap<SessionId, (String, Option<EditDraft>)>,
    composer_restore: Option<(String, String)>,
    pending_fork_refresh: bool,
    pub(crate) session_search: Entity<InputState>,
    pub(crate) trajectory_search: Entity<InputState>,
    trajectory_query_value: String,
    pub(crate) modal: Option<Modal>,
    pub(crate) modal_focus: FocusHandle,
    pub(crate) composer_popup: Option<Entity<gpui_kit::component::menu::PopupMenu>>,
    pub(crate) trajectory_scroll: ListState,
    pub(crate) trajectory_scroll_restore: Cell<Option<ListOffset>>,
    pub(crate) trajectory_follow_tail: Cell<bool>,
    pending_trajectory_query_restore: RefCell<Option<String>>,
    pub(crate) trajectory_list_structure: Cell<Option<(u64, u64, u64, bool)>>,
    pub(crate) details_scroll: ScrollHandle,
    pub(crate) timeline_bounds: Option<Bounds<Pixels>>,
    pub(crate) trajectory_ledger_width: Option<(LayoutGeneration, Pixels)>,
    pub(crate) timeline_drag: Option<TimelineDragState>,
    pub(crate) timeline_hover: Option<TimelineHoverState>,
    pub(crate) request_marker_hover: Option<TrajectoryRequestKey>,
    pub(crate) timeline_model_cache: RefCell<Option<TimelineModelCache>>,
    pub(crate) trajectory_details_layout: TrajectoryDetailsLayoutState,
    pub(crate) trajectory_details_markdown: RefCell<TrajectoryDetailsMarkdownCache>,
    pub(crate) models: Vec<ConfiguredModel>,
    pub(crate) selected_model: usize,
    pub(crate) selected_reasoning_effort: Option<harness::ReasoningEffort>,
    pub(crate) model: String,
    pub(crate) project_store: ProjectStore,
    pub(crate) settings: SettingsStore,
    pub(crate) selected_started_at: Option<Instant>,
    pub(crate) project_sessions: HashMap<PathBuf, Vec<SessionInfo>>,
    pub(crate) project_archived_sessions: HashMap<PathBuf, Vec<SessionInfo>>,
    pub(crate) session_search_documents: HashMap<PathBuf, SessionSearchDocument>,
    session_catalog_indices: HashMap<RuntimeKey, usize>,
    pub(crate) available_update: Option<AvailableUpdate>,
    unread_sessions: HashSet<(ProjectId, SessionId)>,
    runtime_observations: HashMap<RuntimeKey, RuntimeObservation>,
    runtime_subscriptions: HashMap<RuntimeKey, Subscription>,
    runtime_recency: HashMap<RuntimeKey, u64>,
    runtime_access_clock: u64,
    open_generation: u64,
    inflight_session_opens: HashMap<OpenSessionKey, u64>,
    pending_runtime_selection: Option<PendingRuntimeSelection>,
    native_titlebar: NativeTitlebarController,
    view_states: HashMap<RuntimeKey, SessionViewState>,
    _subscriptions: Vec<Subscription>,
}

impl DesktopApp {
    #[allow(
        clippy::expect_used,
        reason = "DesktopStartup contains the ProjectStore-validated active project"
    )]
    pub(crate) fn new(
        startup: DesktopStartup,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let DesktopStartup {
            agent,
            models,
            selected_model,
            project_store,
            active_project,
            settings,
        } = startup;
        let project = project_store
            .project(active_project)
            .expect("active project should exist")
            .clone();
        let model = models[selected_model].label();
        let current_session = agent.session_info().path.clone();
        let current_session_id = agent.session_info().id.clone();
        let runtime_config = config_for_model(&models[selected_model], settings.allow_all_tools());
        let selected_reasoning_effort = runtime_config.model.reasoning_effort;
        let runtime = cx.new(|cx| {
            SessionConnection::new(
                &cx.global::<crate::session::ApplicationHarness>().0,
                agent,
                project.id.as_str().to_owned(),
                project.sessions_dir.clone(),
                SessionDocument::default(),
                runtime_config,
            )
        });
        let runtime_subscription = cx.observe(&runtime, |this, runtime, cx| {
            this.sync_runtime_snapshot(&runtime, cx);
        });
        let initial_runtime_key = (project.id.clone(), current_session_id.clone());
        let mut project_runtimes = HashMap::new();
        project_runtimes.insert(
            project.id.clone(),
            ProjectSessionRuntimes {
                selected: current_session_id.clone(),
                sessions: HashMap::from([(current_session_id, runtime.clone())]),
            },
        );
        let SessionCatalogCache {
            project_sessions,
            session_search_documents,
            session_catalog_indices,
        } = load_session_catalog_cache(&project_store);
        let project_archived_sessions = load_project_archived_sessions(&project_store);
        let viewport = window.viewport_size();
        let mut core = AppState::new(LayoutInput {
            viewport_width: f32::from(viewport.width),
            viewport_height: f32::from(viewport.height),
            rem_size: f32::from(window.rem_size()),
            ..LayoutInput::default()
        });
        core.trajectory.mode = if settings.trajectory_actual_duration() {
            TimelineMode::Duration
        } else {
            TimelineMode::Sequence
        };
        core.workspace.cwd = project.path.clone();
        core.workspace.active_project = active_project;
        core.workspace.expanded_projects = HashSet::from([project.path.clone()]);
        core.workspace.sessions_dir = project.sessions_dir.clone();
        core.session.current = current_session.clone();
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 14)
                .submit_on_enter(true)
                .placeholder("Describe what you want to build")
        });
        let session_search =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search sessions…"));
        let trajectory_search =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search trajectory"));
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                event if is_composer_submit_event(event) => {
                    if this.core.composer.menu.is_none() {
                        this.submit(window, cx);
                    }
                }
                InputEvent::Change => {
                    this.composer_edit_revision = this.composer_edit_revision.saturating_add(1);
                    if this.core.follow_chat_tail {
                        this.schedule_chat_tail(window);
                    }
                    cx.notify();
                }
                _ => {}
            },
        );
        let search_subscription = cx.subscribe_in(
            &session_search,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    if let Some(Modal::SessionSearch { selected }) = &mut this.modal {
                        *selected = 0;
                    }
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => {
                    this.open_selected_session_search_result(window, cx)
                }
                _ => {}
            },
        );
        let trajectory_subscription = cx.subscribe_in(
            &trajectory_search,
            window,
            |this, search, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.trajectory_query_value = search.read(cx).value().to_string();
                    cx.notify();
                }
            },
        );
        input.update(cx, |input, cx| input.focus(window, cx));
        let native_titlebar = NativeTitlebarController::install(window);
        let appearance_subscription = cx.observe_window_appearance(window, |this, window, cx| {
            if this.settings.appearance() == Appearance::System {
                Theme::sync_system_appearance(Some(window), cx);
                cx.notify();
            }
        });
        let bounds_subscription = cx.observe_window_bounds(window, |this, window, cx| {
            this.native_titlebar.sync(window);
            this.sync_window_layout(window, cx);
        });
        // Keep short trajectories anchored below the overview like DSH. Tail following is an
        // independent scroll policy and still moves overflowing ledgers to their newest item.
        let trajectory_scroll = ListState::new(0, ListAlignment::Top, px(1_000.0));
        let trajectory_follow_tail = Cell::new(true);
        let trajectory_scroll_owner = cx.entity().downgrade();
        trajectory_scroll.set_scroll_handler({
            move |event, _, cx| {
                let owner = trajectory_scroll_owner.clone();
                let (is_scrolled, visible_end, count) =
                    (event.is_scrolled, event.visible_range.end, event.count);
                // GPUI holds ListState's mutable borrow throughout this callback.
                cx.defer(move |cx| {
                    let _ = owner.update(cx, |this, _| {
                        let follows = if !is_scrolled {
                            true
                        } else if count > 0 && visible_end == count {
                            this.trajectory_scroll
                                .bounds_for_item(count - 1)
                                .is_some_and(|last| {
                                    let remaining = (last.bottom()
                                        - this.trajectory_scroll.viewport_bounds().bottom())
                                    .max(px(0.0));
                                    remaining <= px(2.0)
                                })
                        } else {
                            false
                        };
                        this.trajectory_follow_tail.set(follows);
                    });
                });
            }
        });
        let app = Self {
            core,
            chat: RefCell::new(ChatViewport::default()),
            html_previews: crate::chat::html_preview::HtmlPreviews::new(window, cx),
            message_presentations: RefCell::new(MessagePresentationStore::default()),
            selected_runtime: runtime,
            project_runtimes,
            input,
            composer_submitting: false,
            composer_edit_revision: 0,
            edit_draft: None,
            relations_hovered: false,
            relations_open: false,
            composer_drafts: HashMap::new(),
            composer_restore: None,
            pending_fork_refresh: false,
            session_search,
            trajectory_search,
            trajectory_query_value: String::new(),
            modal: None,
            modal_focus: cx.focus_handle(),
            composer_popup: None,
            trajectory_scroll,
            trajectory_scroll_restore: Cell::new(None),
            trajectory_follow_tail,
            pending_trajectory_query_restore: RefCell::new(None),
            trajectory_list_structure: Cell::new(None),
            details_scroll: ScrollHandle::new(),
            timeline_bounds: None,
            trajectory_ledger_width: None,
            timeline_drag: None,
            timeline_hover: None,
            request_marker_hover: None,
            timeline_model_cache: RefCell::new(None),
            trajectory_details_layout: TrajectoryDetailsLayoutState::default(),
            trajectory_details_markdown: RefCell::new(TrajectoryDetailsMarkdownCache::default()),
            models,
            selected_model,
            selected_reasoning_effort,
            model,
            project_store,
            settings,
            selected_started_at: None,
            project_sessions,
            project_archived_sessions,
            session_search_documents,
            session_catalog_indices,
            available_update: None,
            unread_sessions: HashSet::new(),
            runtime_observations: HashMap::new(),
            runtime_subscriptions: HashMap::from([(
                initial_runtime_key.clone(),
                runtime_subscription,
            )]),
            runtime_recency: HashMap::from([(initial_runtime_key.clone(), 1)]),
            runtime_access_clock: 1,
            open_generation: 0,
            inflight_session_opens: HashMap::new(),
            pending_runtime_selection: None,
            native_titlebar,
            view_states: HashMap::new(),
            _subscriptions: vec![
                subscription,
                search_subscription,
                trajectory_subscription,
                appearance_subscription,
                bounds_subscription,
            ],
        };
        app.message_presentations
            .borrow_mut()
            .activate(presentation_namespace(
                &initial_runtime_key.0,
                &initial_runtime_key.1,
            ));
        #[cfg(not(test))]
        app.check_for_updates(window, cx);
        app
    }

    fn sync_window_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let viewport = window.viewport_size();
        let mut input = self.core.layout_input;
        input.viewport_width = f32::from(viewport.width);
        input.viewport_height = f32::from(viewport.height);
        input.rem_size = f32::from(window.rem_size());
        input.sidebar_requested = self.core.sidebar_requested;
        input.trajectory_visible = self.core.surface == Surface::Trajectory;
        input.details_visible = self.core.details.selected.is_some();
        self.dispatch(Action::LayoutInputChanged(input), window, cx);
    }

    pub(crate) fn dispatch(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        let effects = self.transition(action);
        run_effects(self, effects, window, cx);
        cx.notify();
    }

    fn transition(&mut self, action: Action) -> Vec<Effect> {
        let previous_generation = self.core.layout_generation;
        let mut effects = reduce(&mut self.core, action);
        if self.core.composer.menu.is_none() {
            self.composer_popup = None;
        }
        if self.core.layout_generation != previous_generation {
            self.chat.borrow().list.remeasure();
            effects.retain(|effect| !matches!(effect, Effect::ApplyChatTail));
        }
        effects
    }

    pub(crate) fn dispatch_local(&mut self, action: Action, cx: &mut Context<Self>) {
        for effect in self.transition(action) {
            let Effect::ApplyChatTail = effect;
            self.chat.borrow().list.scroll_to_end();
        }
        cx.notify();
    }

    pub(crate) fn task_active(&self) -> bool {
        matches!(
            self.core.run,
            RunState::Preparing | RunState::Running { .. }
        )
    }

    pub(crate) fn session_running(&self) -> bool {
        !self.selection_pending() && matches!(self.core.run, RunState::Running { .. })
    }

    pub(crate) fn update_composer_measurement(
        &mut self,
        height: f32,
        cx: &mut Context<Self>,
    ) -> bool {
        if !height.is_finite()
            || height <= 0.0
            || (self.core.layout_input.composer_height - height).abs() < 0.5
        {
            return false;
        }
        let mut input = self.core.layout_input;
        input.composer_height = height;
        let effects = self.transition(Action::LayoutInputChanged(input));
        let restore_tail = effects
            .iter()
            .any(|effect| matches!(effect, Effect::ApplyChatTail));
        debug_assert!(
            effects
                .iter()
                .all(|effect| matches!(effect, Effect::ApplyChatTail)),
            "layout measurement produced a non-layout effect"
        );
        cx.notify();
        restore_tail
    }

    pub(crate) fn update_main_measurement(&mut self, width: f32, cx: &mut Context<Self>) -> bool {
        if !width.is_finite()
            || width <= 0.0
            || (self.core.layout_input.measured_main_width - width).abs() < 0.5
        {
            return false;
        }
        let mut input = self.core.layout_input;
        input.measured_main_width = width;
        let changed_before = self.core.layout_generation;
        let _ = self.transition(Action::LayoutInputChanged(input));
        let changed = self.core.layout_generation != changed_before;
        if changed {
            cx.notify();
        }
        changed
    }

    pub(crate) fn restore_chat_tail_after_layout(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.core.follow_chat_tail {
            self.schedule_chat_tail(window);
            cx.notify();
        }
    }

    pub(crate) fn toggle_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dispatch(Action::CloseTransientOverlays, window, cx);
        self.dispatch(Action::ToggleSidebar, window, cx);
    }

    pub(crate) fn set_trajectory(
        &mut self,
        trajectory: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.save_current_view_state();
        self.dispatch(Action::SetComposerMenu(None), window, cx);
        self.dispatch(
            if trajectory {
                Action::ShowTrajectory
            } else {
                Action::ShowChat
            },
            window,
            cx,
        );
        if trajectory {
            self.chat.borrow_mut().release();
        }
        self.restore_current_view_state(cx);
        cx.notify();
    }

    fn view_state_key(&self) -> Option<RuntimeKey> {
        // Derive the identity from the selected runtime, not the mutable workspace/session
        // projection in `core`. During a cross-project async open the workspace already names the
        // target while the selected runtime still owns the source session; combining those fields
        // would save a chimeric key and lose every edit made while loading.
        self.runtime_location(&self.selected_runtime)
    }

    fn save_current_view_state(&mut self) {
        let Some(key) = self.view_state_key() else {
            return;
        };
        let state = self.view_states.entry(key).or_default();
        state.surface = self.core.surface;
        if self.core.surface == Surface::Trajectory {
            state.trajectory_follow_tail = self.trajectory_follow_tail.get();
            state.trajectory_offset = (!state.trajectory_follow_tail).then(|| {
                self.trajectory_scroll_restore
                    .get()
                    .unwrap_or_else(|| self.trajectory_scroll.logical_scroll_top())
            });
            state
                .trajectory_query
                .clone_from(&self.trajectory_query_value);
            state.selected_details = self.core.details.selected.clone();
            state
                .details_tab_history
                .clone_from(&self.core.details.tab_history);
            state.details_offset = self.details_scroll.offset();
            state.collapsed_turns = self.core.trajectory.collapsed_turns.clone();
            state.collapsed_assistants = self.core.trajectory.collapsed_assistants.clone();
            state.timeline_selection = self
                .core
                .trajectory
                .selected_range
                .map(SavedTimelineRange::capture);
            state.timeline_viewport = self
                .core
                .trajectory
                .visible_range
                .map(SavedTimelineRange::capture);
        } else {
            state.chat_anchor = self.chat.borrow().anchor();
        }
    }

    fn restore_current_view_state(&mut self, _cx: &mut Context<Self>) {
        let state = self
            .view_state_key()
            .and_then(|key| self.view_states.get(&key))
            .cloned()
            .unwrap_or_default();
        if self.core.surface == Surface::Trajectory {
            let _ = self.transition(Action::SetTrajectoryTurnsCollapsed(
                state.collapsed_turns.clone(),
            ));
            let _ = self.transition(Action::SetTrajectoryAssistantsCollapsed(
                state.collapsed_assistants.clone(),
            ));
            let projection = &self.core.session_view.trajectory;
            let mode = self.core.trajectory.mode;
            let lineage = projection.projection_lineage();
            let revision = projection.revision();
            let selection = state
                .timeline_selection
                .and_then(|range| range.restore(mode, lineage, revision));
            let viewport = state
                .timeline_viewport
                .and_then(|range| range.restore(mode, lineage, revision));
            let _ = self.transition(Action::SetTimelineSelection(selection));
            let _ = self.transition(Action::SetTimelineViewport(viewport));
            self.timeline_drag = None;
            self.timeline_hover = None;
            self.request_marker_hover = None;
            self.trajectory_follow_tail
                .set(state.trajectory_follow_tail);
            *self.pending_trajectory_query_restore.borrow_mut() =
                Some(state.trajectory_query.clone());
            self.trajectory_query_value = state.trajectory_query.clone();
            // The new session's row count is not known until its ledger projection is rendered.
            // Applying the offset before then would clamp it against the previous session's list.
            self.trajectory_scroll_restore.set(state.trajectory_offset);
            self.trajectory_list_structure.set(None);
            let selected = state.selected_details.filter(|selected| {
                let trajectory = &self.core.session_view.trajectory;
                match selected {
                    DetailsSelection::Record(id) => trajectory.record_index(id).is_some(),
                    DetailsSelection::Request(key) => trajectory.request_index(key).is_some(),
                }
            });
            let _ = self.transition(Action::RestoreSessionView {
                selected: selected.clone(),
                details_tab_history: state.details_tab_history.clone(),
                follow_chat_tail: matches!(state.chat_anchor, ScrollAnchor::Tail),
            });
            self.details_scroll.set_offset(state.details_offset);
        } else {
            let _ = self.transition(Action::RestoreSessionView {
                selected: None,
                details_tab_history: state.details_tab_history,
                follow_chat_tail: matches!(state.chat_anchor, ScrollAnchor::Tail),
            });
            self.chat.borrow_mut().pending_anchor = Some(state.chat_anchor);
        }
    }

    pub(crate) fn apply_pending_trajectory_query_restore(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(query) = self.pending_trajectory_query_restore.borrow_mut().take() else {
            return;
        };
        if self.trajectory_search.read(cx).value().as_ref() != query.as_str() {
            self.trajectory_search
                .update(cx, |search, cx| search.set_value(query, window, cx));
        }
    }

    pub(crate) fn toggle_tool(
        &mut self,
        index: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_message_presentation(index, Role::Tool, cx);
    }

    pub(crate) fn toggle_reasoning(&mut self, index: usize, cx: &mut Context<Self>) {
        self.toggle_message_presentation(index, Role::Reasoning, cx);
    }

    fn toggle_message_presentation(&mut self, index: usize, role: Role, cx: &mut Context<Self>) {
        let Some(message_id) = self
            .core
            .session_view
            .conversation
            .messages
            .get(index)
            .filter(|message| message.role == role)
            .map(|message| message.key)
        else {
            return;
        };
        if self
            .message_presentations
            .get_mut()
            .toggle_expanded(message_id)
            .is_some_and(|expanded| expanded && self.core.follow_chat_tail)
        {
            self.chat.borrow().list.scroll_to_end();
        }
        cx.notify();
    }

    pub(crate) fn inspect_tool(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .core
            .session_view
            .conversation
            .messages
            .get(index)
            .is_some_and(|message| message.role == Role::Tool)
        {
            self.save_current_view_state();
            let Some(call_id) = self.core.session_view.conversation.messages[index]
                .tool_call_id
                .as_deref()
            else {
                return;
            };
            let Some((trajectory_index, record)) = self
                .core
                .session_view
                .trajectory
                .records
                .iter()
                .enumerate()
                .find(|(_, record)| {
                    matches!(
                        &record.id,
                        TrajectoryItemId::Tool(record_call_id)
                            if record_call_id.as_str() == call_id
                    )
                })
            else {
                return;
            };
            let message_id = record.id.clone();
            let mut effects = self.transition(Action::ShowTrajectory);
            effects.extend(
                self.transition(Action::SelectDetails(Some(DetailsSelection::Record(
                    message_id,
                )))),
            );
            run_effects(self, effects, window, cx);
            self.dispatch_local(Action::SetDetailsTab(DetailsTab::Summary), cx);
            self.details_scroll.set_offset(point(px(0.0), px(0.0)));
            self.dispatch_local(Action::ExpandTrajectoryGroups, cx);
            self.scroll_trajectory_to_record(trajectory_index, cx);
            cx.notify();
        }
    }

    pub(crate) fn rate_message(&mut self, index: usize, positive: bool, cx: &mut Context<Self>) {
        let Some(message_id) = self
            .core
            .session_view
            .conversation
            .messages
            .get(index)
            .filter(|message| message.role == Role::Assistant)
            .map(|message| message.key)
        else {
            return;
        };
        if self
            .message_presentations
            .get_mut()
            .rate(message_id, positive)
            .is_some()
        {
            cx.notify();
        }
    }

    pub(crate) fn handle_root_key(
        &mut self,
        event: &gpui_kit::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let modifiers = event.keystroke.modifiers;
        if event.keystroke.key.eq_ignore_ascii_case("b")
            && modifiers.secondary()
            && !modifiers.alt
            && !modifiers.shift
            && !modifiers.function
        {
            self.toggle_sidebar(window, cx);
            cx.stop_propagation();
            return;
        }
        // The framework dialog/menu owns Escape, Enter, and arrow navigation.
        if self.modal.is_some() || self.core.composer.menu.is_some() {
            return;
        }
        if event.keystroke.key == "escape" {
            if self.edit_draft.is_some() {
                self.cancel_edit(window, cx);
                return;
            }
            self.dismiss_transient(window, cx);
        }
    }

    pub(crate) fn dismiss_transient(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal.is_some() {
            self.close_modal(window, cx);
            return;
        }
        self.cancel_timeline_gesture();
        self.dispatch(Action::DismissTransient, window, cx);
        cx.notify();
    }

    pub(crate) fn set_appearance(
        &mut self,
        appearance: Appearance,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Err(error) = self.settings.set_appearance(appearance) {
            self.notice(format!("Could not save appearance: {error}"));
        }
        match appearance {
            Appearance::System => Theme::sync_system_appearance(Some(window), cx),
            Appearance::Light => Theme::change(ThemeMode::Light, Some(window), cx),
            Appearance::Dark => Theme::change(ThemeMode::Dark, Some(window), cx),
        }
        cx.notify();
    }

    pub(crate) fn set_reduce_motion(&mut self, reduce: bool, cx: &mut Context<Self>) {
        if let Err(error) = self.settings.set_reduce_motion(reduce) {
            self.notice(format!("Could not save motion preference: {error}"));
        }
        cx.notify();
    }

    pub(crate) fn set_trajectory_actual_duration(
        &mut self,
        enabled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Err(error) = self.settings.set_trajectory_actual_duration(enabled) {
            self.notice(format!(
                "Could not save trajectory duration preference: {error}"
            ));
        }
        for state in self.view_states.values_mut() {
            state.timeline_selection = None;
            state.timeline_viewport = None;
        }
        self.dispatch(
            Action::SetTimelineMode(if enabled {
                TimelineMode::Duration
            } else {
                TimelineMode::Sequence
            }),
            window,
            cx,
        );
    }

    pub(crate) fn chat_at_bottom(&self) -> bool {
        let chat = self.chat.borrow();
        chat.list.is_following_tail() || chat.list.is_scrolled_to_end() == Some(true)
    }

    fn schedule_chat_tail(&mut self, _window: &mut Window) {
        self.chat
            .borrow()
            .list
            .set_follow_mode(gpui_kit::FollowMode::Tail);
    }

    pub(crate) fn request_chat_tail(&mut self, window: &mut Window) {
        self.schedule_chat_tail(window);
    }

    pub(crate) fn scroll_chat_to_bottom(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dispatch(Action::Scroll(ScrollIntent::JumpToTail), window, cx);
    }
}

fn is_composer_submit_event(event: &InputEvent) -> bool {
    matches!(event, InputEvent::PressEnter { shift: false, .. })
}

fn message(role: Role, text: String) -> Message {
    Message {
        key: next_message_id(),
        revision: 0,
        role,
        tool_call_id: None,
        title: None,
        text,
        payload: None,
        schema: None,
        pending: false,
        failed: false,
        started_at_ms: Some(now_ms()),
        duration_ms: None,
        turn: 0,
        step: 0,
        request_id: None,
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn config_for_model(model: &ConfiguredModel, allow_all_tools: bool) -> SessionConfig {
    SessionConfig {
        model: model.session_model_config(),
        allow_all_tools,
    }
}

fn safe_file_name(value: &str) -> String {
    let name = value
        .chars()
        .map(|character| {
            if character.is_alphanumeric() || matches!(character, '-' | '_' | ' ') {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    let name = name.trim().trim_matches('-');
    if name.is_empty() {
        "session".into()
    } else {
        name.into()
    }
}

#[cfg(test)]
fn within_bottom_threshold(max_offset: gpui_kit::Pixels, offset_y: gpui_kit::Pixels) -> bool {
    max_offset + offset_y <= px(24.0)
}

#[cfg(test)]
fn update_chat_follow_on_scroll(
    delta_y: gpui_kit::Pixels,
    at_bottom: bool,
    follow_chat_tail: &mut bool,
    unread_stream_updates: &mut usize,
) -> bool {
    if delta_y > px(0.0) && *follow_chat_tail {
        *follow_chat_tail = false;
        true
    } else if delta_y < px(0.0) && at_bottom {
        let changed = !*follow_chat_tail || *unread_stream_updates > 0;
        *follow_chat_tail = true;
        *unread_stream_updates = 0;
        changed
    } else {
        false
    }
}

pub(crate) fn session_age(created_at: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let elapsed = now.saturating_sub(created_at);
    match elapsed {
        0..60 => "now".into(),
        60..3600 => format!("{}m", elapsed / 60),
        3600..86400 => format!("{}h", elapsed / 3600),
        _ => format!("{}d", elapsed / 86400),
    }
}

fn resolve_sidebar_session_status(
    status: &SessionConnectionStatus,
    approval_needed: bool,
    unread: bool,
) -> Option<SidebarSessionStatus> {
    if approval_needed {
        return Some(SidebarSessionStatus::ApprovalNeeded);
    }
    match status {
        SessionConnectionStatus::Creating | SessionConnectionStatus::Configuring => {
            Some(SidebarSessionStatus::Preparing)
        }
        SessionConnectionStatus::Running | SessionConnectionStatus::Settling => {
            Some(SidebarSessionStatus::Running)
        }
        SessionConnectionStatus::Failed(_) => Some(SidebarSessionStatus::Failed),
        SessionConnectionStatus::Idle if unread => Some(SidebarSessionStatus::Unread),
        SessionConnectionStatus::Idle => None,
    }
}

fn has_new_unread_completion(previous: u64, current: u64, selected: bool) -> bool {
    !selected && current > previous
}

fn visible_transcript_update_count(previous: u64, current: u64, selected: bool) -> usize {
    if !selected {
        return 0;
    }
    usize::try_from(current.saturating_sub(previous)).unwrap_or(usize::MAX)
}

pub(crate) fn same_path(left: &Path, right: &Path) -> bool {
    left == right
}

fn presentation_namespace(project_id: &ProjectId, session_id: &SessionId) -> String {
    let project_id = project_id.as_str();
    format!("{}:{project_id}{}", project_id.len(), session_id.as_str())
}

#[cfg(test)]
mod tests;
