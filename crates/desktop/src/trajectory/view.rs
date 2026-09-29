mod model;
mod state;
pub(crate) use model::TimelineModelCache;
use model::*;
use state::*;
pub(crate) use state::{TrajectoryDetailsLayoutState, TrajectoryDetailsMarkdownCache};
mod details;
mod interaction;
mod ledger;
use std::borrow::Borrow;
use std::collections::HashSet;
use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::Input;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ElementExt, Icon, IconName, Sizable};
use gpui_kit::{
    Context, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement, Pixels, Point, Role, ScrollStrategy, ScrollWheelEvent,
    SharedString, StatefulInteractiveElement, Styled, Window, accesskit, div,
    prelude::FluentBuilder, px, relative,
};
use im::{HashSet as ImHashSet, Vector};
use time::{OffsetDateTime, UtcOffset, macros::format_description};

use crate::app::layout::TrajectoryMode;
use crate::app::{DesktopApp, TimelineDragState, TimelineHoverState};
use crate::rendering::assets::DesktopIconName;
use crate::rendering::automation::ids;
use crate::rendering::markdown;
use crate::rendering::streaming_markdown::StreamingMarkdownState;
use crate::rendering::theme::{TrajectoryPalette, metrics, trajectory_palette};
use crate::session::document::EventTimeRef;
use crate::trajectory::timeline::{
    AxisId, AxisRange, DomainRange, RenderCell, TimelineGeometry, TimelineLane, TimelinePoint,
    TimelineSpan,
};
use crate::{
    app::{Action, DetailsSelection, DetailsTab},
    session::{
        ItemStatus, LayoutGeneration, ModelRequestOptions, PromptChangeKind, PromptSnapshot,
        TrajectoryItemId, TrajectoryKind, TrajectoryRecord, TrajectoryRecordDetails,
        TrajectoryRequest, TrajectoryRequestPurpose,
    },
    trajectory::TimelineMode,
};

const TIMELINE_INPUT_TOP: f32 = 6.0;
const TIMELINE_MODEL_TOP: f32 = 20.0;
const TIMELINE_TOOLS_TOP: f32 = 34.0;
const TIMELINE_BAR_OFFSET: f32 = 1.0;
const TIMELINE_BAR_HEIGHT: f32 = 8.0;
const TIMELINE_BAR_GAP_FRACTION: f64 = 0.08;
const TIMELINE_BAR_GAP_MAX_PX: f64 = 1.0;
const TIMELINE_BAR_MIN_WIDTH_PX: f64 = 2.0;
const TIMELINE_CLICK_SLOP: f32 = 3.0;
const TIMELINE_PRIMITIVE_LIMIT: usize = 3_000;
const TIMELINE_EDGE_PAN_MAX_PX: f64 = 48.0;
const TIMELINE_EDGE_PAN_ZONE_FRACTION: f64 = 0.08;
const TIMELINE_EDGE_PAN_STEP_FRACTION: f64 = 0.035;
const DETAILS_MIN_WIDTH: f32 = 320.0;
const DETAILS_MAX_WIDTH: f32 = 720.0;
const DETAILS_DEFAULT_MAX_WIDTH: f32 = 440.0;
const LEDGER_MIN_WIDTH: f32 = 280.0;
const DETAILS_OVERLAY_MAX_WIDTH: f32 = 420.0;
const DETAILS_OVERLAY_FRACTION: f32 = 0.92;
const DETAILS_KEYBOARD_STEP: f32 = 16.0;
const DETAILS_MEASUREMENT_EPSILON: f32 = 0.5;
const COMPACT_LEDGER_MAX_WIDTH: f32 = 620.0;
const TOOL_SUMMARY_PREVIEW_MAX_BYTES: usize = 160;

fn trajectory_ledger_row_height(row: &TimelineLedgerRow) -> f32 {
    match row {
        TimelineLedgerRow::Record(_) => metrics::LEDGER_ROW_HEIGHT,
        TimelineLedgerRow::RequestBoundary { terminal, .. } => {
            if *terminal {
                9.0
            } else {
                0.0
            }
        }
        TimelineLedgerRow::TurnSummary { .. } | TimelineLedgerRow::CallsSummary { .. } => {
            LEDGER_SUMMARY_ROW_HEIGHT
        }
    }
}

fn turn_summary_text(step_count: usize, call_count: usize) -> String {
    format!(
        "… {step_count} step{} · {call_count} tool call{}",
        if step_count == 1 { "" } else { "s" },
        if call_count == 1 { "" } else { "s" },
    )
}

fn calls_summary_text(call_count: usize, tools: &str) -> String {
    let names = (!tools.is_empty()).then(|| format!(" · {tools}"));
    format!(
        "… {call_count} tool call{}{}",
        if call_count == 1 { "" } else { "s" },
        names.unwrap_or_default(),
    )
}

fn sync_trajectory_list_state(
    state: &gpui_kit::ListState,
    item_count: usize,
    structure_changed: bool,
    restore: Option<gpui_kit::ListOffset>,
    follow_tail: bool,
) {
    let current_count = state.item_count();
    if structure_changed {
        let offset = state.logical_scroll_top();
        state.reset_with_uniform_height(item_count, px(metrics::LEDGER_ROW_HEIGHT));
        state.scroll_to(offset);
    } else if item_count > current_count {
        state.splice(current_count..current_count, item_count - current_count);
    } else if item_count < current_count {
        state.splice(item_count..current_count, 0);
    }
    if let Some(restore) = restore {
        state.scroll_to(restore);
    } else if follow_tail {
        state.scroll_to(gpui_kit::ListOffset {
            item_ix: item_count,
            offset_in_item: px(0.0),
        });
    }
}

fn aligned_trajectory_list_offset(
    rows: &TimelineRows,
    target: usize,
    viewport_height: f32,
    alignment: f32,
) -> gpui_kit::ListOffset {
    let Some(target_row) = rows.get(target) else {
        return gpui_kit::ListOffset {
            item_ix: rows.len(),
            offset_in_item: px(0.0),
        };
    };
    let target_height = trajectory_ledger_row_height(&target_row);
    let desired_before =
        ((viewport_height - target_height).max(0.0) * alignment.clamp(0.0, 1.0)).max(0.0);
    let mut item_ix = target;
    let mut available_before = 0.0;
    while item_ix > 0 && available_before < desired_before {
        item_ix -= 1;
        if let Some(row) = rows.get(item_ix) {
            available_before += trajectory_ledger_row_height(&row);
        }
    }
    gpui_kit::ListOffset {
        item_ix,
        offset_in_item: px((available_before - desired_before).max(0.0)),
    }
}

impl DesktopApp {
    pub(crate) fn trajectory_panel(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let query = self
            .trajectory_search
            .read(cx)
            .value()
            .trim()
            .to_lowercase();
        let query_active = !query.is_empty();
        let filter = self.timeline_filter_snapshot(&query);
        let selected = self.core.details.selected.as_ref().and_then(|selected| {
            let trajectory = &self.core.session_view.trajectory;
            match selected {
                DetailsSelection::Record(id) => {
                    trajectory.record_index(id).map(InspectorTarget::Record)
                }
                DetailsSelection::Request(key) => {
                    trajectory.request_index(key).map(InspectorTarget::Request)
                }
            }
        });

        div()
            .id("trajectory-panel")
            .role(Role::TabPanel)
            .accessibility_id(ids::TRAJECTORY_PANEL)
            .aria_label("Trajectory")
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .overflow_hidden()
            .bg(trajectory_palette(cx).background)
            .text_color(trajectory_palette(cx).label_primary)
            .child(self.trajectory_toolbar(&filter.fold_controls, cx))
            .child(self.trajectory_overview(&filter.matched_cells, cx))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_hidden()
                    .child(match selected {
                        Some(target) => self.trajectory_selected_panes(
                            target,
                            &filter.rows,
                            query_active,
                            window,
                            cx,
                        ),
                        None => self
                            .trajectory_ledger(&filter.rows, query_active, cx)
                            .into_any_element(),
                    }),
            )
            .children(self.timeline_drag.is_some().then(|| {
                div()
                    .id("trajectory-timeline-drag-capture")
                    .absolute()
                    .top_0()
                    .right_0()
                    .bottom_0()
                    .left_0()
                    .when(
                        self.timeline_drag.as_ref().is_some_and(|drag| drag.pan),
                        |capture| capture.cursor_grabbing(),
                    )
                    .when(
                        self.timeline_drag.as_ref().is_some_and(|drag| !drag.pan),
                        |capture| capture.cursor_crosshair(),
                    )
                    .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                        this.timeline_mouse_move(event, window, cx);
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseUpEvent, window, cx| {
                            this.timeline_mouse_up(event, window, cx);
                        }),
                    )
                    .on_mouse_up(
                        MouseButton::Right,
                        cx.listener(|this, event: &MouseUpEvent, window, cx| {
                            this.timeline_mouse_up(event, window, cx);
                        }),
                    )
                    .on_mouse_up_out(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseUpEvent, window, cx| {
                            this.timeline_mouse_up(event, window, cx);
                        }),
                    )
                    .on_mouse_up_out(
                        MouseButton::Right,
                        cx.listener(|this, event: &MouseUpEvent, window, cx| {
                            this.timeline_mouse_up(event, window, cx);
                        }),
                    )
            }))
            .children(self.trajectory_details_layout.is_dragging().then(|| {
                div()
                    .id("trajectory-details-drag-capture")
                    .absolute()
                    .top_0()
                    .right_0()
                    .bottom_0()
                    .left_0()
                    .cursor_col_resize()
                    .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                        if this
                            .trajectory_details_layout
                            .drag_to(f32::from(event.position.x))
                        {
                            cx.notify();
                        }
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseUpEvent, _, cx| {
                            if this.trajectory_details_layout.end_drag() {
                                cx.notify();
                            }
                        }),
                    )
                    .on_mouse_up_out(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseUpEvent, _, cx| {
                            if this.trajectory_details_layout.end_drag() {
                                cx.notify();
                            }
                        }),
                    )
            }))
    }

    fn trajectory_selected_panes(
        &self,
        target: InspectorTarget,
        rows: &TimelineRows,
        query_active: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let mode = self.core.layout.trajectory;
        let layout_generation = self.core.layout_generation;
        let fallback_split_width = self.core.layout.main_width;
        let details_width = self.trajectory_details_layout.details_width(
            mode,
            layout_generation,
            fallback_split_width,
        );
        let split_entity = cx.entity().clone();
        let details_entity = split_entity.clone();
        let ledger = div()
            .min_w(px(0.0))
            .min_h(px(0.0))
            .overflow_hidden()
            .when(mode == TrajectoryMode::Split, |element| {
                element.flex_1().min_w(px(LEDGER_MIN_WIDTH))
            })
            .when(mode == TrajectoryMode::Overlay, |element| {
                element.size_full()
            })
            .child(self.trajectory_ledger(rows, query_active, cx));
        let details = div()
            .id("trajectory-v1-details-pane")
            .relative()
            .flex_none()
            .h_full()
            .w(px(details_width))
            .occlude()
            .when(mode == TrajectoryMode::Overlay, |element| {
                element.absolute().top_0().right_0().bottom_0().shadow_xl()
            })
            .on_prepaint(move |bounds, _, cx| {
                details_entity.update(cx, |this, cx| {
                    if this.core.layout_generation == layout_generation
                        && this
                            .trajectory_details_layout
                            .observe_details_width(layout_generation, f32::from(bounds.size.width))
                    {
                        cx.notify();
                    }
                });
            })
            .child(
                div()
                    .size_full()
                    .overflow_hidden()
                    .child(self.trajectory_details(target, window, cx)),
            )
            .child(self.trajectory_details_resize_handle(cx));

        div()
            .id("trajectory-v1-selected-panes")
            .relative()
            .flex()
            .size_full()
            .min_w(px(0.0))
            .min_h(px(0.0))
            .overflow_hidden()
            .child(ledger)
            .child(details)
            .on_prepaint(move |bounds, _, cx| {
                split_entity.update(cx, |this, cx| {
                    if this.core.layout_generation == layout_generation
                        && this
                            .trajectory_details_layout
                            .observe_split_width(layout_generation, f32::from(bounds.size.width))
                    {
                        cx.notify();
                    }
                });
            })
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                if this
                    .trajectory_details_layout
                    .drag_to(f32::from(event.position.x))
                {
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| {
                    if this.trajectory_details_layout.end_drag() {
                        cx.notify();
                    }
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| {
                    if this.trajectory_details_layout.end_drag() {
                        cx.notify();
                    }
                }),
            )
            .into_any_element()
    }

    fn trajectory_details_resize_handle(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = trajectory_palette(cx);
        let generation = self.core.layout_generation;
        let split_width = self
            .trajectory_details_layout
            .split_width(generation, self.core.layout.main_width);
        let current_width = self
            .trajectory_details_layout
            .measured_details_width(generation)
            .unwrap_or_else(|| {
                self.trajectory_details_layout.details_width(
                    self.core.layout.trajectory,
                    generation,
                    self.core.layout.main_width,
                )
            });
        let max_width =
            (split_width - LEDGER_MIN_WIDTH).clamp(DETAILS_MIN_WIDTH, DETAILS_MAX_WIDTH);
        div()
            .id("trajectory-v1-details-resize-handle")
            .absolute()
            .top_0()
            .bottom_0()
            .left(px(-4.0))
            .w(px(8.0))
            .cursor_col_resize()
            .occlude()
            .tab_index(0)
            .role(Role::Splitter)
            .aria_label("Resize event details")
            .aria_orientation(accesskit::Orientation::Vertical)
            .aria_min_numeric_value(DETAILS_MIN_WIDTH as f64)
            .aria_max_numeric_value(max_width as f64)
            .aria_numeric_value(current_width as f64)
            .aria_numeric_value_step(DETAILS_KEYBOARD_STEP as f64)
            .hover(|style| style.bg(colors.primary.opacity(0.08)))
            .focus(|style| style.bg(colors.primary.opacity(0.12)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    if event.click_count >= 2 {
                        if this.trajectory_details_layout.reset() {
                            cx.notify();
                        }
                        return;
                    }
                    let generation = this.core.layout_generation;
                    let split_width = this
                        .trajectory_details_layout
                        .split_width(generation, this.core.layout.main_width);
                    let current_width = this
                        .trajectory_details_layout
                        .measured_details_width(generation)
                        .unwrap_or_else(|| {
                            this.trajectory_details_layout.details_width(
                                this.core.layout.trajectory,
                                generation,
                                this.core.layout.main_width,
                            )
                        });
                    this.trajectory_details_layout.begin_drag(
                        f32::from(event.position.x),
                        current_width,
                        split_width,
                    );
                    cx.notify();
                }),
            )
            .on_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, _, cx| {
                let delta = match event.keystroke.key.as_str() {
                    "left" => DETAILS_KEYBOARD_STEP,
                    "right" => -DETAILS_KEYBOARD_STEP,
                    _ => return,
                };
                let generation = this.core.layout_generation;
                let split_width = this
                    .trajectory_details_layout
                    .split_width(generation, this.core.layout.main_width);
                let current_width = this
                    .trajectory_details_layout
                    .measured_details_width(generation)
                    .unwrap_or_else(|| {
                        this.trajectory_details_layout.details_width(
                            this.core.layout.trajectory,
                            generation,
                            this.core.layout.main_width,
                        )
                    });
                if this
                    .trajectory_details_layout
                    .step(delta, current_width, split_width)
                {
                    cx.notify();
                }
                cx.stop_propagation();
            }))
    }

    fn trajectory_toolbar(
        &self,
        fold_controls: &TimelineFoldControlSnapshot,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let colors = trajectory_palette(cx);
        let actual_duration = self.core.trajectory.mode != TimelineMode::Sequence;
        let all_turns_collapsed = fold_controls.all_turns_collapsed;
        let all_assistants_collapsed = fold_controls.all_assistants_collapsed;
        div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .h(px(metrics::LEDGER_TOOLBAR_HEIGHT))
            .px(px(6.0))
            .min_w(px(0.0))
            .overflow_hidden()
            .border_b_1()
            .border_color(colors.border_l2)
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(2.0))
                    .child(
                        Button::new("toggle-trajectory-duration")
                            .icon(DesktopIconName::Clock)
                            .label("Duration")
                            .xsmall()
                            .compact()
                            .ghost()
                            .text_color(if actual_duration {
                                colors.label_primary
                            } else {
                                colors.label_tertiary
                            })
                            .when(actual_duration, |button| button.bg(colors.hover))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.set_trajectory_actual_duration(!actual_duration, window, cx);
                            })),
                    )
                    .child(
                        Button::new("toggle-trajectory-turns")
                            .label(if all_turns_collapsed {
                                "⊞  Turns"
                            } else {
                                "⊟  Turns"
                            })
                            .xsmall()
                            .compact()
                            .ghost()
                            .text_color(colors.label_tertiary)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let collapsible =
                                    this.core.session_view.trajectory.collapsible_turns();
                                let mut next_turns = this.core.trajectory.collapsed_turns.clone();
                                next_turns.retain(|turn| collapsible.contains(turn));
                                for turn in collapsible.iter() {
                                    if all_turns_collapsed {
                                        next_turns.remove(turn);
                                    } else {
                                        next_turns.insert(*turn);
                                    }
                                }
                                this.dispatch(
                                    Action::SetTrajectoryTurnsCollapsed(next_turns),
                                    window,
                                    cx,
                                );
                            })),
                    )
                    .child(
                        Button::new("toggle-trajectory-calls")
                            .label(if all_assistants_collapsed {
                                "⊞  Calls"
                            } else {
                                "⊟  Calls"
                            })
                            .xsmall()
                            .compact()
                            .ghost()
                            .text_color(colors.label_tertiary)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let collapsible =
                                    this.core.session_view.trajectory.collapsible_assistants();
                                let mut next_assistants =
                                    this.core.trajectory.collapsed_assistants.clone();
                                next_assistants.retain(|assistant| collapsible.contains(assistant));
                                for assistant in collapsible.iter() {
                                    if all_assistants_collapsed {
                                        next_assistants.remove(assistant);
                                    } else {
                                        next_assistants.insert(assistant.clone());
                                    }
                                }
                                this.dispatch(
                                    Action::SetTrajectoryAssistantsCollapsed(next_assistants),
                                    window,
                                    cx,
                                );
                            })),
                    ),
            )
            .child(
                div()
                    .ml_auto()
                    .w(px(164.0))
                    .min_w(px(84.0))
                    .flex_shrink(1.0)
                    .child(
                        Input::new(&self.trajectory_search)
                            .accessibility_id(ids::TRAJECTORY_SEARCH_INPUT)
                            .aria_label("Search trajectory")
                            .with_size(px(22.0))
                            .text_size(px(12.0))
                            .prefix(IconName::Search)
                            .cleanable(true),
                    ),
            )
    }

    fn trajectory_overview(
        &self,
        matching: &TimelineCellMatches,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let colors = trajectory_palette(cx);
        self.ensure_timeline_model_cache();
        let cache = self.timeline_model_cache.borrow();
        let model = cache.as_ref().and_then(|cache| cache.model.as_ref());
        let timeline_empty = model.is_none_or(|model| model.cells.is_empty());
        let turn_boundaries = cache
            .as_ref()
            .and_then(|cache| {
                Some(timeline_turn_boundary_fractions(
                    &self.core.session_view.trajectory.records,
                    cache.geometry.as_ref()?,
                    cache.model.as_ref()?.viewport,
                ))
            })
            .unwrap_or_default();
        let focused_cells = cache
            .as_ref()
            .and_then(|cache| cache.focus.as_ref())
            .map(|focus| Arc::clone(&focus.focused_cells));
        let committed_selection = cache
            .as_ref()
            .and_then(|cache| cache.display_selection(self.core.trajectory.selected_range));
        let entity = cx.entity().clone();
        let display_selection = self
            .timeline_drag
            .as_ref()
            .filter(|drag| !drag.pan)
            .and_then(|drag| {
                model
                    .filter(|model| drag.initial_viewport.axis == model.axis)
                    .map(|model| AxisRange {
                        axis: model.axis,
                        range: DomainRange::new(drag.start_value, drag.current_value)
                            .clamp_to(model.domain),
                    })
            })
            .or(committed_selection);
        let selection = display_selection.and_then(|selection| {
            model.map(|model| normalized_range(selection.range, model.viewport))
        });
        let selection_dragging = self.timeline_drag.as_ref().is_some_and(|drag| !drag.pan);
        let timeline_panning = self.timeline_drag.as_ref().is_some_and(|drag| drag.pan);
        div()
            .flex()
            .h(px(50.0))
            .overflow_hidden()
            .bg(colors.code_background)
            .border_b_1()
            .border_color(colors.border_l2)
            .child(
                div()
                    .relative()
                    .flex_none()
                    .w(px(44.0))
                    .h_full()
                    .pl_1()
                    .pr(px(3.0))
                    .items_end()
                    .overflow_hidden()
                    .border_r_1()
                    .border_color(colors.border_l2)
                    .text_size(px(10.0))
                    .line_height(px(10.0))
                    .text_color(colors.label_caption)
                    .child(timeline_lane_label("Input", TIMELINE_INPUT_TOP))
                    .child(timeline_lane_label("Model", TIMELINE_MODEL_TOP))
                    .child(timeline_lane_label("Tools", TIMELINE_TOOLS_TOP)),
            )
            .child(
                div()
                    .id("trajectory-timeline")
                    .relative()
                    .flex_1()
                    .h_full()
                    .min_w(px(0.0))
                    .overflow_hidden()
                    .tab_index(0)
                    .when(timeline_panning, |timeline| timeline.cursor_grabbing())
                    .when(!timeline_panning, |timeline| timeline.cursor_crosshair())
                    .children(selection.map(|(left, _width)| {
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left_0()
                            .w(relative(left.max(0.0) as f32))
                            .bg(colors.background.opacity(0.58))
                    }))
                    .children(selection.map(|(left, width)| {
                        let right = (left + width).clamp(0.0, 1.0);
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left(relative(right as f32))
                            .w(relative((1.0 - right) as f32))
                            .bg(colors.background.opacity(0.58))
                    }))
                    .children(selection.map(|(left, width)| {
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left(relative(left as f32))
                            .w(relative(width.max(0.002) as f32))
                            .bg(colors.primary.opacity(if selection_dragging {
                                0.18
                            } else {
                                0.12
                            }))
                    }))
                    .children(
                        model
                            .zip(cache.as_ref().and_then(|cache| cache.geometry.as_ref()))
                            .map(|(model, geometry)| {
                                self.timeline_lane(
                                    TimelineLane::Input,
                                    geometry,
                                    model,
                                    focused_cells.as_deref(),
                                    matching,
                                    cx,
                                )
                            }),
                    )
                    .children(
                        model
                            .zip(cache.as_ref().and_then(|cache| cache.geometry.as_ref()))
                            .map(|(model, geometry)| {
                                self.timeline_lane(
                                    TimelineLane::Model,
                                    geometry,
                                    model,
                                    focused_cells.as_deref(),
                                    matching,
                                    cx,
                                )
                            }),
                    )
                    .children(
                        model
                            .zip(cache.as_ref().and_then(|cache| cache.geometry.as_ref()))
                            .map(|(model, geometry)| {
                                self.timeline_lane(
                                    TimelineLane::Tools,
                                    geometry,
                                    model,
                                    focused_cells.as_deref(),
                                    matching,
                                    cx,
                                )
                            }),
                    )
                    .children(turn_boundaries.into_iter().map(|fraction| {
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left(relative(fraction as f32))
                            .w(px(1.0))
                            .bg(colors.border_l2)
                    }))
                    .children(selection.map(|(left, width)| {
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left(relative(left as f32))
                            .w(relative(width.max(0.002) as f32))
                            .when(selection_dragging, |edge| edge.border_l_2().border_r_2())
                            .when(!selection_dragging, |edge| edge.border_l_3().border_r_3())
                            .border_color(colors.primary)
                    }))
                    .children(timeline_empty.then(|| {
                        div()
                            .absolute()
                            .top_0()
                            .right_0()
                            .bottom_0()
                            .left_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(13.0))
                            .text_color(colors.label_caption)
                            .child("No timing data")
                    }))
                    .children(
                        self.timeline_hover
                            .as_ref()
                            .filter(|hover| {
                                hover.record_id.is_none()
                                    && self.timeline_drag.is_none()
                                    && model.is_some_and(|model| hover.axis == model.axis)
                            })
                            .map(|hover| {
                                div()
                                    .absolute()
                                    .top_0()
                                    .bottom_0()
                                    .left(relative(hover.fraction.clamp(0.0, 1.0) as f32))
                                    .w(px(2.0))
                                    .bg(colors.primary)
                            }),
                    )
                    .on_prepaint(move |bounds, _, cx| {
                        entity.update(cx, |this, _| this.timeline_bounds = Some(bounds));
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, window, cx| {
                            this.timeline_mouse_down(event, false, window, cx)
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|this, event: &MouseDownEvent, window, cx| {
                            this.timeline_mouse_down(event, true, window, cx)
                        }),
                    )
                    .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                        this.timeline_mouse_move(event, window, cx)
                    }))
                    .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                        if !*hovered {
                            let changed = this.timeline_hover.take().is_some();
                            // The mouse-up-out handler owns gesture completion.
                            // Leaving the hitbox must not cancel a valid drag.
                            if changed {
                                cx.notify();
                            }
                        }
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseUpEvent, window, cx| {
                            this.timeline_mouse_up(event, window, cx)
                        }),
                    )
                    .on_mouse_up(
                        MouseButton::Right,
                        cx.listener(|this, event: &MouseUpEvent, window, cx| {
                            this.timeline_mouse_up(event, window, cx)
                        }),
                    )
                    .on_mouse_up_out(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseUpEvent, window, cx| {
                            this.timeline_mouse_up(event, window, cx)
                        }),
                    )
                    .on_mouse_up_out(
                        MouseButton::Right,
                        cx.listener(|this, event: &MouseUpEvent, window, cx| {
                            this.timeline_mouse_up(event, window, cx)
                        }),
                    )
                    .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, window, cx| {
                        this.timeline_wheel(event, window, cx)
                    }))
                    .on_key_down(cx.listener(
                        |this, event: &gpui_kit::KeyDownEvent, window, cx| {
                            if event.keystroke.key.as_str() == "escape"
                                && this.core.trajectory.selected_range.is_some()
                            {
                                this.dispatch(Action::SetTimelineSelection(None), window, cx);
                                cx.stop_propagation();
                            }
                        },
                    )),
            )
    }

    #[allow(
        clippy::expect_used,
        reason = "timeline cache presence is checked before mutable access"
    )]
    fn ensure_timeline_model_cache(&self) {
        let projection = &self.core.session_view.trajectory;
        let revision = projection.revision();
        let change_revision = projection.change_revision();
        let document_generation = projection.projection_lineage();
        let mode = self.core.trajectory.mode;
        let axis = AxisId {
            document_generation,
            geometry_revision: revision,
            mode,
        };
        let viewport = self.core.trajectory.visible_range;
        let selection = self.core.trajectory.selected_range;
        let render_width_px = self
            .timeline_bounds
            .map(|bounds| f64::from(f32::from(bounds.size.width)).max(1.0))
            .unwrap_or(1_500.0);
        let mut cache = self.timeline_model_cache.borrow_mut();
        let mut rebuild = cache
            .as_ref()
            .is_none_or(|cache| !cache.projection_matches(document_generation, mode));
        let mut focus_changed = false;
        if !rebuild {
            match cache
                .as_mut()
                .expect("timeline cache was checked above")
                .sync_projection(projection)
            {
                Some(changed) => focus_changed = changed,
                None => rebuild = true,
            }
        }
        if rebuild
            || cache
                .as_ref()
                .is_none_or(|cache| !cache.geometry_matches(axis))
        {
            let retained_search = cache.take().and_then(|previous| {
                previous
                    .geometry
                    .as_ref()
                    .is_some_and(|geometry| {
                        geometry.axis.document_generation == document_generation
                    })
                    .then_some(previous.search)
                    .flatten()
            });
            *cache = Some(TimelineModelCache::new(
                TimelineCacheIdentity {
                    axis,
                    change_revision,
                },
                &projection.records,
                TimelineView {
                    viewport,
                    selection,
                    render_width_px,
                },
                retained_search,
            ));
            focus_changed = false;
        } else if cache.as_ref().is_some_and(|cache| {
            cache.viewport != viewport || (cache.render_width_px - render_width_px).abs() >= 1.0
        }) {
            cache
                .as_mut()
                .expect("timeline cache was checked above")
                .sync_ranges(viewport, render_width_px);
        }
        if let Some(cache) = cache.as_mut() {
            cache.sync_focus(&projection.records, selection, focus_changed);
        }
    }

    fn timeline_filter_snapshot(&self, query: &str) -> TimelineFilterSnapshot {
        self.ensure_timeline_model_cache();
        self.timeline_model_cache
            .borrow_mut()
            .as_mut()
            .map(|cache| {
                cache.search_snapshot(
                    &self.core.session_view.trajectory,
                    query,
                    self.core.trajectory.fold_revision,
                    &self.core.trajectory.collapsed_turns,
                    &self.core.trajectory.collapsed_assistants,
                )
            })
            .unwrap_or(TimelineFilterSnapshot {
                matched_cells: TimelineCellMatches::All,
                rows: TimelineRows::All(0),
                fold_controls: TimelineFoldControlSnapshot::default(),
            })
    }

    fn with_timeline_model<T>(&self, project: impl FnOnce(&TimelineModel) -> T) -> Option<T> {
        self.ensure_timeline_model_cache();
        let cache = self.timeline_model_cache.borrow();
        cache
            .as_ref()
            .and_then(|cache| cache.model.as_ref())
            .map(project)
    }

    fn with_timeline_geometry<T>(&self, project: impl FnOnce(&TimelineGeometry) -> T) -> Option<T> {
        self.ensure_timeline_model_cache();
        let cache = self.timeline_model_cache.borrow();
        cache
            .as_ref()
            .and_then(|cache| cache.geometry.as_ref())
            .map(project)
    }

    fn timeline_lane(
        &self,
        lane: TimelineLane,
        geometry: &TimelineGeometry,
        model: &TimelineModel,
        focused_cells: Option<&HashSet<usize>>,
        matching: &TimelineCellMatches,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let hovered = self
            .timeline_hover
            .as_ref()
            .filter(|hover| hover.axis == model.axis)
            .and_then(|hover| hover.record_id.as_ref());
        let selected = self
            .core
            .details
            .selected
            .as_ref()
            .and_then(DetailsSelection::record);
        let cell_for_id = |id: &TrajectoryItemId| {
            self.core
                .session_view
                .trajectory
                .record_index(id)
                .and_then(|index| geometry.render_cell_for_record(&model.cells, index))
        };
        let hovered_cell = hovered.and_then(cell_for_id);
        let selected_cell = selected.and_then(cell_for_id);
        let paint = TimelinePaintContext {
            render_width_px: model.render_width_px,
            focused_cells,
            matching,
            hovered_cell,
            selected_cell,
        };
        let emphasized_cells = [hovered_cell, selected_cell];
        let ordinary = model
            .cells
            .iter()
            .enumerate()
            .filter(move |(ordinal, cell)| {
                cell.lane == lane && !emphasized_cells.contains(&Some(*ordinal))
            });
        let emphasized = emphasized_cells
            .into_iter()
            .flatten()
            .enumerate()
            .filter(|(position, cell_index)| {
                emphasized_cells[..*position]
                    .iter()
                    .all(|prior| prior != &Some(*cell_index))
            })
            .filter_map(|(_, cell_index)| {
                model.cells.get(cell_index).map(|cell| (cell_index, cell))
            })
            .filter(|(_, cell)| cell.lane == lane);
        div()
            .absolute()
            .top(px(timeline_lane_top(lane)))
            .left_0()
            .right_0()
            .h(px(10.0))
            .children(
                ordinary
                    .chain(emphasized)
                    .map(|(ordinal, cell)| self.timeline_block(ordinal, cell, &paint, cx)),
            )
    }

    fn timeline_block(
        &self,
        ordinal: usize,
        cell: &RenderCell,
        paint: &TimelinePaintContext<'_>,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let colors = trajectory_palette(cx);
        let selected = paint.selected_cell == Some(ordinal);
        let hovered = paint.hovered_cell == Some(ordinal);
        let selected_index = selected
            .then_some(
                self.core
                    .details
                    .selected
                    .as_ref()
                    .and_then(DetailsSelection::record),
            )
            .flatten()
            .and_then(|id| self.core.session_view.trajectory.record_index(id));
        let hovered_index = hovered
            .then(|| {
                self.timeline_hover
                    .as_ref()
                    .and_then(|hover| hover.record_id.as_ref())
            })
            .flatten()
            .and_then(|id| self.core.session_view.trajectory.record_index(id));
        let record_index = hovered_index
            .or(selected_index)
            .or_else(|| cell.ids.last().copied())
            .unwrap_or_default();
        let record = &self.core.session_view.trajectory.records[record_index];
        let focused = paint
            .focused_cells
            .is_none_or(|focused| focused.contains(&ordinal));
        let matched = paint.matching.contains(ordinal);
        let color = record_color(record, colors);
        let mut tooltip = record_tooltip(record);
        if cell.clustered {
            tooltip.push_str(&format!("\n{} items in this range", cell.ids.len()));
        }
        let width_px = paint.render_width_px.max(1.0);
        let cell_width_px = (cell.end_px - cell.start_px).max(0.0);
        let gap_px = timeline_bar_gap_px(cell_width_px);
        let left = (cell.start_px + gap_px) / width_px;
        let width = (cell_width_px - gap_px * 2.0).max(TIMELINE_BAR_MIN_WIDTH_PX) / width_px;
        let execution = nested_segment_geometry(cell);
        let emphasized = hovered || selected;
        let opacity = timeline_block_opacity(record.kind, focused, matched, emphasized);
        let background_opacity = if record.kind == TrajectoryKind::Assistant && execution.is_some()
        {
            opacity * 0.54
        } else {
            opacity
        };
        // Search dims unmatched bars, but DSH keeps hover/current outlines fully legible so the
        // focused item never disappears while inspecting a filtered trajectory.
        let ring_opacity = if selected { 1.0 } else { 0.8 };
        div()
            .id(("timeline-record-v2", record.source_seq))
            .absolute()
            .left(relative(left as f32))
            .top(px(TIMELINE_BAR_OFFSET))
            .w(relative(width as f32))
            .h(px(TIMELINE_BAR_HEIGHT))
            .rounded(px(1.0))
            .bg(color.opacity(background_opacity))
            .children(execution.map(|(left, width)| {
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(relative(left as f32))
                    .w(relative(width as f32))
                    .rounded(px(1.0))
                    .bg(if record.kind == TrajectoryKind::Tool {
                        colors.label_primary.opacity(opacity * 0.14)
                    } else {
                        color.opacity(opacity)
                    })
            }))
            .children(emphasized.then(|| {
                div()
                    .absolute()
                    .top(px(-1.0))
                    .bottom(px(-1.0))
                    .left(px(-1.0))
                    .right(px(-1.0))
                    .rounded(px(2.0))
                    .border_1()
                    .border_color(colors.code_background.opacity(ring_opacity))
            }))
            .children(emphasized.then(|| {
                div()
                    .absolute()
                    .top(px(-2.0))
                    .bottom(px(-2.0))
                    .left(px(-2.0))
                    .right(px(-2.0))
                    .rounded(px(3.0))
                    .border_1()
                    .border_color(colors.primary.opacity(ring_opacity))
            }))
            .cursor_pointer()
            .tooltip(move |window, cx| {
                Tooltip::new(tooltip.clone())
                    .text_size(px(11.0))
                    .line_height(px(16.0))
                    .build(window, cx)
            })
            .into_any_element()
    }
}

#[allow(clippy::too_many_arguments)]
fn trajectory_event_cell(
    record: &TrajectoryRecord,
    turn_start: bool,
    compact: bool,
    outside: bool,
    opacity: f32,
    kind_color: gpui_kit::Hsla,
    colors: TrajectoryPalette,
    active_turn: bool,
    selected: bool,
) -> gpui_kit::AnyElement {
    let kind = kind_label(record.kind).to_uppercase();
    let content = if compact {
        let tooltip = kind.clone();
        let icon = match record.kind {
            TrajectoryKind::System => IconName::Settings,
            TrajectoryKind::User | TrajectoryKind::Steering => IconName::User,
            TrajectoryKind::Context => IconName::Info,
            TrajectoryKind::Assistant => IconName::Bot,
            TrajectoryKind::Tool => IconName::SquareTerminal,
            TrajectoryKind::Compaction => IconName::Minimize,
        };
        div()
            .flex()
            .items_center()
            .justify_end()
            .w_full()
            .pl(px(28.0))
            .pr(px(3.0))
            .child(
                div()
                    .id(("trajectory-kind-icon-v1", record.source_seq))
                    .flex()
                    .items_center()
                    .justify_center()
                    .w(px(19.0))
                    .h(px(19.0))
                    .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                    .child(
                        Icon::new(icon)
                            .size_3()
                            .text_color(kind_color.opacity(opacity)),
                    ),
            )
            .into_any_element()
    } else {
        div()
            .flex()
            .items_center()
            .justify_end()
            .w_full()
            .pl(px(36.0))
            .pr(px(4.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_end()
                    .w(px(76.0))
                    .overflow_hidden()
                    .child(
                        div()
                            .px_2()
                            .h(px(19.0))
                            .flex()
                            .items_center()
                            .rounded(px(4.0))
                            .text_size(px(10.0))
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(kind_color.opacity(opacity))
                            .bg(kind_color.opacity(if outside { 0.035 } else { 0.1 }))
                            .max_w_full()
                            .overflow_hidden()
                            .child(kind),
                    ),
            )
            .into_any_element()
    };
    div()
        .relative()
        .flex()
        .items_center()
        .w(px(if compact { 50.0 } else { 122.0 }))
        .h_full()
        .overflow_hidden()
        .text_xs()
        .text_color(colors.label_caption.opacity(opacity))
        .children(active_turn.then(|| {
            div()
                .absolute()
                .left_0()
                .top_0()
                .bottom_0()
                .w(px(2.0))
                .bg(colors.primary.opacity(opacity * 0.22))
        }))
        .children(selected.then(|| {
            div().absolute().left_0().top_0().bottom_0().w(px(3.0)).bg(
                if matches!(record.status, ItemStatus::Failed | ItemStatus::Aborted) {
                    colors.error
                } else {
                    colors.primary
                },
            )
        }))
        .children((turn_start && record.turn.is_some()).then(|| {
            div()
                .absolute()
                .top_0()
                .left_0()
                .h(px(12.0))
                .px(px(5.0))
                .flex()
                .items_center()
                .rounded_br(px(2.0))
                .bg(if active_turn {
                    colors.primary.opacity(0.08)
                } else {
                    colors.code_background
                })
                .text_size(px(8.0))
                .text_color(if active_turn {
                    colors.primary.opacity(opacity)
                } else {
                    colors.label_tertiary.opacity(opacity)
                })
                .child(if compact {
                    format!("#{}", record.turn.unwrap_or_default())
                } else {
                    format!("Turn {}", record.turn.unwrap_or_default())
                })
        }))
        .child(content)
        .into_any_element()
}

fn timeline_turn_boundary_fractions(
    records: &Vector<Arc<TrajectoryRecord>>,
    geometry: &TimelineGeometry,
    viewport: DomainRange,
) -> Vec<f64> {
    let viewport_width = viewport.width().max(f64::EPSILON);
    let mut boundaries = Vec::new();
    for (index, record) in records.iter().enumerate() {
        let Some(turn) = record.turn else { continue };
        if records
            .get(index.wrapping_sub(1))
            .is_some_and(|previous| previous.turn == Some(turn))
        {
            continue;
        }
        let Some(range) = geometry.range_for(&index) else {
            continue;
        };
        let start = range.range.start;
        if start <= geometry.domain.start || start < viewport.start || start > viewport.end {
            continue;
        }
        let fraction = ((start - viewport.start) / viewport_width).clamp(0.0, 1.0);
        if boundaries
            .last()
            .is_none_or(|previous: &f64| (*previous - fraction).abs() > f64::EPSILON)
        {
            boundaries.push(fraction);
        }
    }
    boundaries
}

fn minimum_timeline_selection_width(
    domain: DomainRange,
    viewport: DomainRange,
    span_count: usize,
) -> f64 {
    if span_count == 0 {
        return viewport.width().min(domain.width()).max(f64::EPSILON);
    }
    (domain.width() / span_count as f64)
        .min(viewport.width())
        .max(f64::EPSILON)
}

fn timeline_block_opacity(
    kind: TrajectoryKind,
    focused: bool,
    matched: bool,
    emphasized: bool,
) -> f32 {
    if !matched {
        0.14
    } else if emphasized {
        1.0
    } else if !focused {
        0.2
    } else if matches!(kind, TrajectoryKind::Assistant | TrajectoryKind::Tool) {
        1.0
    } else {
        0.78
    }
}

fn timeline_lane_label(label: &'static str, top: f32) -> gpui_kit::AnyElement {
    div()
        .absolute()
        .top(px(top + 1.0))
        .left_0()
        .right_0()
        .flex()
        .flex_none()
        .items_center()
        .justify_end()
        .h(px(8.0))
        .w_full()
        .overflow_hidden()
        .child(div().max_w_full().truncate().child(label))
        .into_any_element()
}

fn timeline_lane_top(lane: TimelineLane) -> f32 {
    match lane {
        TimelineLane::Input => TIMELINE_INPUT_TOP,
        TimelineLane::Model => TIMELINE_MODEL_TOP,
        TimelineLane::Tools => TIMELINE_TOOLS_TOP,
    }
}

fn timeline_lane_at(local_y: f32) -> Option<TimelineLane> {
    match local_y {
        7.0..=15.0 => Some(TimelineLane::Input),
        21.0..=29.0 => Some(TimelineLane::Model),
        35.0..=43.0 => Some(TimelineLane::Tools),
        _ => None,
    }
}

fn nested_segment_geometry(cell: &RenderCell) -> Option<(f64, f64)> {
    let (start, end) = cell.nested?;
    let cell_width = (cell.end_px - cell.start_px).max(1.0);
    let local_left = ((start - cell.start_px) / cell_width).clamp(0.0, 1.0);
    let available = 1.0 - local_left;
    if available <= 0.0 {
        return None;
    }
    let local_width = ((end - start) / cell_width).max(0.002).min(available);
    (local_width > 0.0).then_some((local_left, local_width))
}

fn focus_scroll_target(
    positions: &[usize],
    rows: &TimelineRows,
    viewport_height: f32,
) -> Option<(usize, ScrollStrategy)> {
    let first = positions.first().copied()?;
    let focused_height = positions
        .iter()
        .filter_map(|position| rows.get(*position))
        .map(|row| trajectory_ledger_row_height(&row))
        .sum::<f32>();
    Some(
        if focused_height > viewport_height.max(metrics::LEDGER_ROW_HEIGHT) {
            (first, ScrollStrategy::Top)
        } else {
            let midpoint = focused_height / 2.0;
            let mut accumulated = 0.0;
            let target = positions
                .iter()
                .copied()
                .find(|position| {
                    accumulated += rows
                        .get(*position)
                        .as_ref()
                        .map_or(0.0, trajectory_ledger_row_height);
                    accumulated >= midpoint
                })
                .unwrap_or(first);
            (target, ScrollStrategy::Center)
        },
    )
}

fn should_clear_selection_for_record(
    source: TrajectorySelectionSource,
    focused: Option<&HashSet<usize>>,
    record_index: usize,
) -> bool {
    match source {
        // A direct bar click replaces the range focus with an item focus. A ledger click within
        // the focused range is only a details navigation and therefore keeps the DSH dimming and
        // range boundaries intact.
        TrajectorySelectionSource::Timeline => true,
        TrajectorySelectionSource::Ledger => {
            focused.is_some_and(|focused| !focused.contains(&record_index))
        }
    }
}

#[cfg(test)]
fn timeline_model<R: Borrow<TrajectoryRecord>>(
    records: &[R],
    mode: TimelineMode,
    viewport: Option<(f64, f64)>,
) -> Option<TimelineModel> {
    let axis = AxisId {
        document_generation: 1,
        geometry_revision: 1,
        mode,
    };
    let geometry = timeline_geometry(records, axis)?;
    Some(project_timeline(
        &geometry,
        viewport.map(domain_range).unwrap_or(geometry.domain),
        1_500.0,
        records.len(),
    ))
}

#[cfg(test)]
fn timeline_geometry<R: Borrow<TrajectoryRecord>>(
    records: &[R],
    axis: AxisId,
) -> Option<TimelineGeometry> {
    timeline_geometry_from_iter(records.iter(), axis)
}

fn timeline_geometry_from_iter<'a, R: Borrow<TrajectoryRecord> + 'a>(
    records: impl Iterator<Item = &'a R>,
    axis: AxisId,
) -> Option<TimelineGeometry> {
    let spans = records
        .enumerate()
        .map(|(index, record)| timeline_span(index, record.borrow()))
        .collect::<Vec<_>>();
    if spans.is_empty() {
        return None;
    }
    Some(TimelineGeometry::build(axis, spans))
}

fn timeline_span(index: usize, record: &TrajectoryRecord) -> TimelineSpan {
    let started = record.timing.started.as_ref().map(timeline_point);
    let completed = record.timing.completed.as_ref().map(timeline_point);
    let nested = match record.kind {
        TrajectoryKind::Tool => record
            .timing
            .execution_started
            .as_ref()
            .zip(record.timing.execution_finished.as_ref()),
        TrajectoryKind::Assistant => record
            .timing
            .first_token
            .as_ref()
            .zip(record.timing.completed.as_ref()),
        _ => None,
    }
    .map(|(start, end)| (timeline_point(start), timeline_point(end)));
    TimelineSpan {
        id: index,
        lane: record.lane(),
        sequence: index as u64,
        started,
        completed,
        duration_ms: record
            .timing
            .duration_ns()
            .map(|duration| duration as f64 / 1_000_000.0),
        nested,
    }
}

fn timeline_point(time: &EventTimeRef) -> TimelinePoint {
    TimelinePoint {
        wall_ms: time.wall_time_ms() as f64,
        clock_id: time.clock_id().to_owned(),
        monotonic_ns: time.monotonic_ns(),
    }
}

fn project_timeline(
    geometry: &TimelineGeometry,
    viewport: DomainRange,
    width_px: f64,
    _record_count: usize,
) -> TimelineModel {
    let viewport = viewport.clamp_to(geometry.domain);
    let width_px = width_px.max(1.0);
    let cells = geometry.render_model(viewport, width_px, TIMELINE_PRIMITIVE_LIMIT);
    TimelineModel {
        axis: geometry.axis,
        domain: geometry.domain,
        viewport,
        render_width_px: width_px,
        cells,
    }
}

#[cfg(test)]
fn domain_range(range: (f64, f64)) -> DomainRange {
    DomainRange::new(range.0, range.1)
}

fn normalized_range(range: DomainRange, viewport: DomainRange) -> (f64, f64) {
    let span = viewport.width().max(f64::EPSILON);
    let left = ((range.start - viewport.start) / span).clamp(0.0, 1.0);
    let right = ((range.end - viewport.start) / span).clamp(0.0, 1.0);
    (left, (right - left).max(0.002))
}

fn resolved_axis_range(range: Option<AxisRange>, geometry: &TimelineGeometry) -> Option<AxisRange> {
    let range = range?;
    // Geometry revisions describe a newer projection of the same semantic axis, not a different
    // interaction space. DSH keeps a numeric range while live assistant/tool timing arrives, so
    // safely rebind it within the same session lineage and mode and clamp it to the new domain.
    // A different lineage or mode has unrelated coordinates and must still be rejected.
    let compatible_axis = range.axis.document_generation == geometry.axis.document_generation
        && range.axis.mode == geometry.axis.mode;
    compatible_axis.then(|| AxisRange {
        axis: geometry.axis,
        range: range.range.clamp_to(geometry.domain),
    })
}

fn record_color(record: &TrajectoryRecord, colors: TrajectoryPalette) -> gpui_kit::Hsla {
    if matches!(
        record.status,
        ItemStatus::Failed | ItemStatus::Aborted | ItemStatus::Unknown
    ) {
        return colors.error;
    }
    match record.kind {
        TrajectoryKind::System => colors.system_foreground,
        TrajectoryKind::User | TrajectoryKind::Steering => colors.user_foreground,
        TrajectoryKind::Context => colors.context_foreground,
        TrajectoryKind::Assistant | TrajectoryKind::Compaction => colors.assistant_foreground,
        TrajectoryKind::Tool => colors.tool_foreground,
    }
}

fn kind_label(kind: TrajectoryKind) -> &'static str {
    match kind {
        TrajectoryKind::System => "System",
        TrajectoryKind::User => "User",
        TrajectoryKind::Context => "Context",
        TrajectoryKind::Steering => "Steering",
        TrajectoryKind::Assistant => "Assistant",
        TrajectoryKind::Tool => "Tool",
        TrajectoryKind::Compaction => "Compaction",
    }
}

fn status_label(status: ItemStatus) -> &'static str {
    match status {
        ItemStatus::Pending | ItemStatus::Running => "Pending",
        ItemStatus::Completed => "Completed",
        ItemStatus::Failed | ItemStatus::Aborted => "Failed",
        ItemStatus::Denied => "Denied",
        ItemStatus::NotExecuted => "Not executed",
        ItemStatus::Unknown => "Unknown side effects",
    }
}

fn record_location(record: &TrajectoryRecord) -> String {
    match (record.turn, record.step) {
        (Some(turn), Some(step)) => format!("T{turn} · S{step}"),
        (Some(turn), None) => format!("T{turn}"),
        _ => "Session".into(),
    }
}

fn request_location(request: &TrajectoryRequest) -> String {
    match (request.turn, request.step) {
        (Some(turn), Some(step)) => format!("T{turn} · S{step}"),
        (Some(turn), None) => format!("T{turn}"),
        _ => "Session".into(),
    }
}

fn request_purpose_label(purpose: TrajectoryRequestPurpose) -> &'static str {
    match purpose {
        TrajectoryRequestPurpose::Assistant => "Assistant generation",
        TrajectoryRequestPurpose::Compaction => "Compaction",
    }
}

fn row_summary(record: &TrajectoryRecord) -> String {
    let first_line = |value: &str| value.lines().next().unwrap_or_default().trim().to_owned();
    match record.kind {
        TrajectoryKind::Tool => {
            let arguments = record
                .payload
                .as_deref()
                .map(first_line)
                .unwrap_or_default();
            let output = first_line(&record.text);
            match (arguments.is_empty(), output.is_empty()) {
                (false, false) => format!("{} {}  →  {}", record.title, arguments, output),
                (false, true) => format!("{} {}", record.title, arguments),
                (true, false) => format!("{}  →  {}", record.title, output),
                (true, true) => record.title.clone(),
            }
        }
        TrajectoryKind::Assistant if record.text.trim().is_empty() => "(tool call only)".into(),
        _ if record.text.trim().is_empty() => record.title.clone(),
        _ => first_line(&record.text),
    }
}

fn tool_request_column_width(ledger_width: f32, compact: bool) -> f32 {
    let event_width = if compact { 50.0 } else { 122.0 };
    let available = (ledger_width - event_width - 44.0).max(0.0);
    (available * 0.58).clamp(180.0, 480.0).min(available)
}

fn trajectory_record_preview(
    record: &TrajectoryRecord,
    colors: TrajectoryPalette,
    opacity: f32,
    tool_request_width: Option<f32>,
) -> gpui_kit::AnyElement {
    if record.kind != TrajectoryKind::Tool {
        return div()
            .flex_1()
            .min_w(px(0.0))
            .pl_2()
            .truncate()
            .text_sm()
            .text_color(colors.label_primary.opacity(opacity))
            .child(row_summary(record))
            .into_any_element();
    }

    let first_line = |value: &str| value.lines().next().unwrap_or_default().trim().to_owned();
    let request = record
        .payload
        .as_deref()
        .map(first_line)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "—".into());
    let result = if record.text.trim().is_empty() {
        match record.status {
            ItemStatus::Pending | ItemStatus::Running => "Pending".into(),
            _ => "—".into(),
        }
    } else {
        first_line(&record.text)
    };
    div()
        .flex()
        .flex_1()
        .min_w(px(0.0))
        .pl_2()
        .items_center()
        .text_sm()
        .text_color(colors.label_primary.opacity(opacity))
        .child(
            div()
                .when(tool_request_width.is_none(), |column| column.flex_1())
                .when_some(tool_request_width, |column, width| {
                    column.flex_none().w(px(width))
                })
                .min_w(px(0.0))
                .truncate()
                .font_family("monospace")
                .text_color(colors.label_secondary.opacity(opacity))
                .child(request),
        )
        .child(
            div()
                .flex_none()
                .px_2()
                .text_xs()
                .text_color(colors.label_caption.opacity(opacity))
                .child("→"),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .truncate()
                .text_color(colors.label_primary.opacity(opacity))
                .child(result),
        )
        .into_any_element()
}

fn record_tooltip(record: &TrajectoryRecord) -> String {
    let mut parts = vec![kind_label(record.kind).to_uppercase()];
    if let Some(started) = record.timing.started.as_ref() {
        let started_at = format_clock(started.wall_time_ms());
        parts.push(if let Some(duration) = record.timing.duration_ns() {
            let completed_at = started
                .wall_time_ms()
                .saturating_add((duration / 1_000_000) as i64);
            format!("{started_at} → {}", format_clock(completed_at))
        } else {
            format!("Started {started_at}")
        });
    }
    let mut timing = record
        .timing
        .duration_ns()
        .map(|duration| format!("Total {}", format_elapsed_duration(Some(duration))))
        .into_iter()
        .collect::<Vec<_>>();
    if record.kind == TrajectoryKind::Assistant
        && let (Some(ttft), Some(decoding)) =
            (record.timing.ttft_ns(), record.timing.generation_ns())
    {
        timing.push(format!(
            "TTFT {} · Decoding {}",
            format_elapsed_duration(Some(ttft)),
            format_elapsed_duration(Some(decoding))
        ));
    }
    if !timing.is_empty() {
        parts.push(timing.join(" · "));
    }
    parts.join("\n")
}

fn format_clock(wall_time_ms: i64) -> String {
    let Ok(nanoseconds) = i128::from(wall_time_ms).checked_mul(1_000_000).ok_or(()) else {
        return "Not recorded".into();
    };
    let Ok(timestamp) = OffsetDateTime::from_unix_timestamp_nanos(nanoseconds) else {
        return "Not recorded".into();
    };
    let offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
    timestamp
        .to_offset(offset)
        .format(format_description!(
            "[hour]:[minute]:[second].[subsecond digits:3]"
        ))
        .unwrap_or_else(|_| "Not recorded".into())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DetailsTabDescriptor {
    tab: DetailsTab,
    label: &'static str,
}

const fn details_tab(tab: DetailsTab, label: &'static str) -> DetailsTabDescriptor {
    DetailsTabDescriptor { tab, label }
}

fn relevant_record_tabs(
    record: &TrajectoryRecord,
    details: Option<&TrajectoryRecordDetails>,
) -> Vec<DetailsTabDescriptor> {
    match record.kind {
        TrajectoryKind::System => {
            let mut tabs = vec![];
            if matches!(
                details,
                Some(TrajectoryRecordDetails::PromptChange { kind, .. })
                    if *kind != PromptChangeKind::Initial
            ) {
                tabs.push(details_tab(DetailsTab::Diff, "Diff"));
            }
            tabs.push(details_tab(DetailsTab::SystemPrompt, "System Prompt"));
            // DSH keeps the tab contract stable even when the request recorded an empty tool
            // catalog. The body then explains that there are no tools instead of moving tabs as
            // the selected record changes.
            tabs.push(details_tab(DetailsTab::Tools, "Tools"));
            tabs
        }
        TrajectoryKind::User | TrajectoryKind::Steering | TrajectoryKind::Context => vec![
            details_tab(DetailsTab::Summary, "Summary"),
            details_tab(DetailsTab::Preview, "Preview"),
            details_tab(DetailsTab::Raw, "Raw"),
        ],
        TrajectoryKind::Assistant => vec![
            details_tab(DetailsTab::Summary, "Summary"),
            details_tab(DetailsTab::Preview, "Preview"),
            details_tab(DetailsTab::Raw, "Raw"),
        ],
        TrajectoryKind::Tool => {
            let mut tabs = vec![details_tab(DetailsTab::Summary, "Summary")];
            if record
                .payload
                .as_deref()
                .is_some_and(|payload| !payload.is_empty())
            {
                tabs.push(details_tab(DetailsTab::Payload, "Payload"));
            }
            if !record.text.is_empty() {
                tabs.push(details_tab(DetailsTab::Result, "Result"));
            }
            // Schema is a stable Tool tab in DSH. Missing historical/schema data is represented
            // by the empty state inside the tab, not by changing the inspector navigation.
            tabs.push(details_tab(DetailsTab::Schema, "Schema"));
            tabs.push(details_tab(DetailsTab::Timing, "Timing"));
            tabs
        }
        TrajectoryKind::Compaction => vec![
            details_tab(DetailsTab::Summary, "Summary"),
            details_tab(DetailsTab::Raw, "Raw Output"),
        ],
    }
}

fn relevant_request_tabs(request: &TrajectoryRequest) -> Vec<DetailsTabDescriptor> {
    let mut tabs = vec![details_tab(DetailsTab::Summary, "Summary")];
    if request.options.is_some() {
        tabs.push(details_tab(DetailsTab::Options, "Options"));
    }
    tabs.push(details_tab(DetailsTab::Usage, "Usage"));
    tabs.push(details_tab(DetailsTab::Timing, "Timing"));
    tabs
}

fn prompt_snapshot(details: Option<&TrajectoryRecordDetails>) -> Option<&PromptSnapshot> {
    match details? {
        TrajectoryRecordDetails::PromptChange { current, .. } => Some(current),
        TrajectoryRecordDetails::Tool { prompt, .. } => Some(prompt),
    }
}

fn prompt_diff_panel(
    details: Option<&TrajectoryRecordDetails>,
    colors: TrajectoryPalette,
) -> gpui_kit::AnyElement {
    let Some(TrajectoryRecordDetails::PromptChange {
        kind,
        current,
        previous: Some(previous),
    }) = details
    else {
        return empty_detail("No prompt diff recorded", colors);
    };
    let mut body = div().flex().flex_col().gap_3();
    if matches!(
        kind,
        PromptChangeKind::System | PromptChangeKind::SystemAndTools
    ) {
        body = body
            .child(section_title("Previous system prompt", colors))
            .child(code_panel(previous.instructions(), colors))
            .child(section_title("Current system prompt", colors))
            .child(code_panel(current.instructions(), colors));
    }
    if matches!(
        kind,
        PromptChangeKind::Tools | PromptChangeKind::SystemAndTools
    ) {
        body = body
            .child(section_title("Previous tools", colors))
            .child(code_panel(previous.tools_json(), colors))
            .child(section_title("Current tools", colors))
            .child(code_panel(current.tools_json(), colors));
    }
    body.into_any_element()
}

fn prompt_tools_panel(prompt: &PromptSnapshot, colors: TrajectoryPalette) -> gpui_kit::AnyElement {
    if prompt.tool_count() == 0 {
        return empty_detail("No tools in this request", colors);
    }
    div()
        .flex()
        .flex_col()
        .gap_3()
        .children(
            prompt
                .tool_schemas()
                .enumerate()
                .flat_map(|(index, (name, schema))| {
                    [
                        section_title(
                            name.unwrap_or(if index == 0 { "Tool" } else { "Unnamed tool" }),
                            colors,
                        ),
                        code_panel(schema, colors),
                    ]
                }),
        )
        .into_any_element()
}

fn usage_summary(usage: harness::TokenUsage, colors: TrajectoryPalette) -> gpui_kit::Div {
    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(detail_pair(
            "Input",
            &format!("{} tok", usage.input_tokens()),
            colors,
        ))
        .child(detail_pair(
            "Cached",
            &format!("{} tok", usage.cache_read_input_tokens),
            colors,
        ))
        .child(detail_pair(
            "Cache created",
            &format!("{} tok", usage.cache_write_input_tokens),
            colors,
        ))
        .child(detail_pair(
            "Other",
            &format!("{} tok", usage.uncached_input_tokens),
            colors,
        ))
        .child(detail_pair(
            "Output",
            &format!("{} tok", usage.total_output_tokens()),
            colors,
        ))
        .child(detail_pair(
            "Reasoning",
            &format!("{} tok", usage.reasoning_output_tokens),
            colors,
        ))
        .child(detail_pair(
            "Content",
            &format!("{} tok", usage.output_tokens),
            colors,
        ))
}

fn request_options_details(
    options: &ModelRequestOptions,
    colors: TrajectoryPalette,
) -> gpui_kit::AnyElement {
    let reason = match options.reason {
        harness::RequestHeaderReason::Initial => "Initial",
        harness::RequestHeaderReason::Resume => "Resume",
        harness::RequestHeaderReason::Change => "Change",
    };
    let mut body = div()
        .flex()
        .flex_col()
        .gap_3()
        .child(detail_pair("Reason", reason, colors))
        .child(detail_pair("Model", &options.model, colors));
    if let Some(effort) = options.reasoning_effort.as_deref() {
        body = body.child(detail_pair("Reasoning effort", effort, colors));
    }
    if let Some(maximum) = options.max_output_tokens {
        body = body.child(detail_pair(
            "Max output tokens",
            &maximum.to_string(),
            colors,
        ));
    }
    body = body.child(section_title("Session configuration", colors));
    if let Some(model) = options.session_config.model.model_id.as_deref() {
        body = body.child(detail_pair("Configured model", model, colors));
    }
    if let Some(effort) = options.session_config.model.reasoning_effort {
        body = body.child(detail_pair(
            "Configured reasoning effort",
            effort.as_str(),
            colors,
        ));
    }
    body.child(detail_pair(
        "Allow all tools",
        if options.session_config.allow_all_tools {
            "Yes"
        } else {
            "No"
        },
        colors,
    ))
    .into_any_element()
}

fn request_usage_details(
    request: &TrajectoryRequest,
    colors: TrajectoryPalette,
) -> gpui_kit::AnyElement {
    let mut body = div().flex().flex_col().gap_3();
    body = body.child(section_title("This request", colors));
    body = match request.usage {
        Some(usage) => body.child(usage_summary(usage, colors)),
        None => body.child(empty_detail("Usage unavailable", colors)),
    };
    body = body.child(section_title("Session cumulative", colors));
    body = match request.cumulative_usage {
        Some(usage) => body.child(usage_summary(usage, colors)),
        None => body.child(empty_detail("Usage unavailable", colors)),
    };
    body.into_any_element()
}

fn detail_pair(label: &str, value: &str, colors: TrajectoryPalette) -> gpui_kit::AnyElement {
    div()
        .flex()
        .justify_between()
        .gap_4()
        .text_sm()
        .child(
            div()
                .text_color(colors.label_tertiary)
                .child(label.to_owned()),
        )
        .child(div().text_right().child(value.to_owned()))
        .into_any_element()
}

fn empty_detail(label: &str, colors: TrajectoryPalette) -> gpui_kit::AnyElement {
    div()
        .text_sm()
        .text_color(colors.label_tertiary)
        .child(label.to_owned())
        .into_any_element()
}

fn section_title(title: &str, colors: TrajectoryPalette) -> gpui_kit::AnyElement {
    div()
        .mt_3()
        .pt_3()
        .border_t_1()
        .border_color(colors.border_l2)
        .text_sm()
        .text_color(colors.label_secondary)
        .child(title.to_owned())
        .into_any_element()
}

fn code_panel(text: &str, colors: TrajectoryPalette) -> gpui_kit::AnyElement {
    div()
        .p_3()
        .rounded(px(6.0))
        .bg(colors.code_background)
        .text_sm()
        .child(text.to_owned())
        .into_any_element()
}

fn format_assistant_duration(nanoseconds: Option<u64>) -> String {
    let Some(nanoseconds) = nanoseconds else {
        return "Not recorded".into();
    };
    let milliseconds = nanoseconds as f64 / 1_000_000.0;
    if milliseconds < 1_000.0 {
        format!("{milliseconds:.0} ms")
    } else if milliseconds < 10_000.0 {
        format!("{:.2} s", milliseconds / 1_000.0)
    } else {
        format!("{:.1} s", milliseconds / 1_000.0)
    }
}

fn format_elapsed_duration(nanoseconds: Option<u64>) -> String {
    let Some(nanoseconds) = nanoseconds else {
        return "—".into();
    };
    let milliseconds = (nanoseconds as f64 / 1_000_000.0).round() as u64;
    let digits = milliseconds.to_string();
    let mut formatted = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            formatted.push(',');
        }
        formatted.push(digit);
    }
    format!("{formatted} ms")
}

fn format_started(record: &TrajectoryRecord, unix: bool) -> String {
    record
        .timing
        .started
        .as_ref()
        .map(|time| format_wall(time.wall_time_ms(), unix))
        .unwrap_or_else(|| "Not available".into())
}

fn format_wall(wall_time_ms: i64, unix: bool) -> String {
    if unix {
        return format!("{:.3}", wall_time_ms as f64 / 1_000.0);
    }
    let Ok(nanoseconds) = i128::from(wall_time_ms).checked_mul(1_000_000).ok_or(()) else {
        return "Not available".into();
    };
    let Ok(timestamp) = OffsetDateTime::from_unix_timestamp_nanos(nanoseconds) else {
        return "Not recorded".into();
    };
    let offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
    timestamp
        .to_offset(offset)
        .format(format_description!(
            "[year]-[month]-[day] [hour]:[minute]:[second].[subsecond digits:3]"
        ))
        .unwrap_or_else(|_| "Not available".into())
}

fn format_timing_point(
    time: Option<&EventTimeRef>,
    unix: bool,
    record: &TrajectoryRecord,
) -> String {
    time.map(|time| format_wall(time.wall_time_ms(), unix))
        .unwrap_or_else(|| execution_missing(record))
}

fn timing_duration(record: &TrajectoryRecord) -> String {
    if record.kind != TrajectoryKind::Assistant {
        return format_elapsed_duration(record.timing.duration_ns());
    }
    if !assistant_timing_recorded(record) {
        return "Not recorded".into();
    }
    if record.timing.started.is_none() {
        return "Step start unavailable".into();
    }
    if record.timing.completed.is_none() {
        return "Pending".into();
    }
    format_assistant_duration(record.timing.duration_ns())
}

fn assistant_timing_recorded(record: &TrajectoryRecord) -> bool {
    record.timing.started.is_some()
        || record.timing.first_token.is_some()
        || record.timing.completed.is_some()
}

fn assistant_ttft(record: &TrajectoryRecord) -> String {
    if !assistant_timing_recorded(record) {
        "Not recorded".into()
    } else if record.timing.started.is_none() {
        "Step start unavailable".into()
    } else if record.timing.first_token.is_none() {
        "First token unavailable".into()
    } else {
        format_assistant_duration(record.timing.ttft_ns())
    }
}

fn assistant_generation(record: &TrajectoryRecord) -> String {
    if !assistant_timing_recorded(record) || record.timing.first_token.is_none() {
        "First token unavailable".into()
    } else if record.timing.completed.is_none() {
        "Pending".into()
    } else {
        format_assistant_duration(record.timing.generation_ns())
    }
}

fn assistant_throughput(record: &TrajectoryRecord) -> String {
    let Some(usage) = record.usage else {
        return "Usage unavailable".into();
    };
    let output_tokens = usage.total_output_tokens();
    if !assistant_timing_recorded(record) || record.timing.first_token.is_none() {
        return "First token unavailable".into();
    }
    if record.timing.completed.is_none() {
        return "Pending".into();
    }
    let generation = record.timing.generation_ns().unwrap_or_default();
    if generation == 0 {
        return "Duration too short".into();
    }
    format!(
        "{:.1} tok/s",
        output_tokens as f64 / (generation as f64 / 1_000_000_000.0)
    )
}

fn execution_missing(record: &TrajectoryRecord) -> String {
    match record.status {
        ItemStatus::Denied | ItemStatus::NotExecuted => "Not executed".into(),
        ItemStatus::Unknown => "Unknown".into(),
        _ => "Not recorded".into(),
    }
}

#[cfg(test)]
mod tests;
