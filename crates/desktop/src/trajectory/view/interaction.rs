use super::*;

impl DesktopApp {
    pub(super) fn select_trajectory(
        &mut self,
        index: usize,
        source: TrajectorySelectionSource,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(record_id) = self
            .core
            .session_view
            .trajectory
            .records
            .get(index)
            .map(|record| record.id.clone())
        else {
            return;
        };
        self.ensure_timeline_model_cache();
        let clear_selection = {
            let cache = self.timeline_model_cache.borrow();
            let focused = cache
                .as_ref()
                .and_then(|cache| cache.focus.as_ref())
                .map(|focus| focus.record_indices.as_ref());
            should_clear_selection_for_record(source, focused, index)
        };
        self.pan_timeline_to_record(&record_id, window, cx);
        if clear_selection {
            self.dispatch(Action::SetTimelineSelection(None), window, cx);
        }
        self.dispatch(
            Action::SelectDetails(Some(DetailsSelection::Record(record_id))),
            window,
            cx,
        );
        self.details_scroll
            .set_offset(gpui_kit::point(px(0.0), px(0.0)));
        self.scroll_trajectory_to_record(index, cx);
    }

    /// DSH treats inspector hierarchy links as navigation, not as a request to keep the target
    /// hidden inside a folded summary. Expand only the groups that own the target before the
    /// canonical selection/scroll path runs.
    pub(super) fn reveal_and_select_trajectory(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(record) = self.core.session_view.trajectory.records.get(index) else {
            return;
        };
        let turn = record.turn;
        let record_id = record.id.clone();
        let owning_assistant = match self.core.session_view.trajectory.record_details(&record_id) {
            Some(TrajectoryRecordDetails::Tool { request_key, .. }) => self
                .core
                .session_view
                .trajectory
                .request_by_key(request_key)
                .and_then(|request| request.result.clone()),
            _ => None,
        };
        if let Some(turn) = turn
            && self.core.trajectory.collapsed_turns.contains(&turn)
        {
            self.dispatch(Action::ToggleTrajectoryTurn(turn), window, cx);
        }
        if self
            .core
            .trajectory
            .collapsed_assistants
            .contains(&record_id)
        {
            self.dispatch(Action::ToggleTrajectoryAssistant(record_id), window, cx);
        }
        if let Some(assistant) = owning_assistant
            && self
                .core
                .trajectory
                .collapsed_assistants
                .contains(&assistant)
        {
            self.dispatch(Action::ToggleTrajectoryAssistant(assistant), window, cx);
        }
        self.select_trajectory(index, TrajectorySelectionSource::Ledger, window, cx);
    }

    pub(crate) fn scroll_trajectory_to_record(&self, index: usize, cx: &mut Context<Self>) {
        let query = self
            .trajectory_search
            .read(cx)
            .value()
            .trim()
            .to_lowercase();
        let rows = self.timeline_filter_snapshot(&query).rows;
        self.sync_trajectory_ledger_list(&rows, !query.is_empty());
        if let Some(row) =
            rows.position(|candidate| self.trajectory_row_represents(candidate, index))
        {
            self.scroll_trajectory_list_row(&rows, row, ScrollStrategy::Center);
            cx.notify();
        }
    }

    pub(super) fn scroll_trajectory_range_into_view(
        &self,
        range: AxisRange,
        cx: &mut Context<Self>,
    ) {
        let Some(focused) = self.with_timeline_geometry(|geometry| geometry.selection(range).items)
        else {
            return;
        };
        if focused.is_empty() {
            return;
        }
        let query = self
            .trajectory_search
            .read(cx)
            .value()
            .trim()
            .to_lowercase();
        let rows = self.timeline_filter_snapshot(&query).rows;
        self.sync_trajectory_ledger_list(&rows, !query.is_empty());
        let positions = (0..rows.len())
            .filter_map(|position| {
                let row = rows.get(position)?;
                row.intersects(&focused, &self.core.session_view.trajectory.records)
                    .then_some(position)
            })
            .collect::<Vec<_>>();
        let viewport_height = f32::from(self.trajectory_scroll.viewport_bounds().size.height);
        let Some((target, strategy)) = focus_scroll_target(&positions, &rows, viewport_height)
        else {
            return;
        };
        self.scroll_trajectory_list_row(&rows, target, strategy);
        cx.notify();
    }

    pub(super) fn scroll_trajectory_list_row(
        &self,
        rows: &TimelineRows,
        target: usize,
        strategy: ScrollStrategy,
    ) {
        self.trajectory_follow_tail.set(false);
        match strategy {
            ScrollStrategy::Top => self.trajectory_scroll.scroll_to(gpui_kit::ListOffset {
                item_ix: target,
                offset_in_item: px(0.0),
            }),
            ScrollStrategy::Center | ScrollStrategy::Bottom => {
                let viewport_height =
                    f32::from(self.trajectory_scroll.viewport_bounds().size.height);
                let alignment = if matches!(strategy, ScrollStrategy::Center) {
                    0.5
                } else {
                    1.0
                };
                self.trajectory_scroll
                    .scroll_to(aligned_trajectory_list_offset(
                        rows,
                        target,
                        viewport_height,
                        alignment,
                    ));
            }
            ScrollStrategy::Nearest => self.trajectory_scroll.scroll_to_reveal_item(target),
        }
    }

    pub(super) fn trajectory_row_represents(
        &self,
        row: &TimelineLedgerRow,
        record_index: usize,
    ) -> bool {
        row.represents(record_index, &self.core.session_view.trajectory.records)
    }

    pub(super) fn pan_timeline_to_record(
        &mut self,
        record_id: &TrajectoryItemId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_timeline_model_cache();
        let viewport = {
            let cache = self.timeline_model_cache.borrow();
            let Some(cache) = cache.as_ref() else { return };
            let Some(record_index) = self.core.session_view.trajectory.record_index(record_id)
            else {
                return;
            };
            let Some(target) = cache
                .geometry
                .as_ref()
                .and_then(|geometry| geometry.range_for(&record_index))
            else {
                return;
            };
            let Some(model) = cache.model.as_ref() else {
                return;
            };
            let viewport = model.viewport.pan_to_reveal(target.range, model.domain);
            (viewport != model.viewport).then_some(AxisRange {
                axis: model.axis,
                range: viewport,
            })
        };
        if let Some(viewport) = viewport {
            self.dispatch(Action::SetTimelineViewport(Some(viewport)), window, cx);
        }
    }

    pub(super) fn timeline_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        pan: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .with_timeline_model(|model| model.cells.is_empty())
            .unwrap_or(true)
        {
            return;
        }
        if event.click_count >= 2 {
            self.timeline_drag = None;
            self.dispatch(Action::SetTimelineSelection(None), window, cx);
            cx.stop_propagation();
            return;
        }
        let Some(value) = self.timeline_value(event.position.x) else {
            return;
        };
        let record_id = (!pan)
            .then(|| self.timeline_record_id(event.position))
            .flatten();
        let Some(initial_viewport) = self.with_timeline_model(|model| AxisRange {
            axis: model.axis,
            range: model.viewport,
        }) else {
            return;
        };
        self.timeline_drag = Some(TimelineDragState {
            pan,
            start_value: value,
            current_value: value,
            start_x: f32::from(event.position.x),
            record_id,
            initial_viewport,
        });
        cx.notify();
        cx.stop_propagation();
    }

    pub(super) fn timeline_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.update_timeline_hover(event.position, cx);
        let Some(drag) = self.timeline_drag.clone() else {
            return;
        };
        if self.with_timeline_model(|model| model.axis) != Some(drag.initial_viewport.axis) {
            self.timeline_drag = None;
            cx.notify();
            return;
        }
        if drag.pan {
            let Some(value) =
                self.timeline_value_in_viewport(event.position.x, Some(drag.initial_viewport))
            else {
                return;
            };
            let viewport = self.with_timeline_model(|model| AxisRange {
                axis: model.axis,
                range: drag
                    .initial_viewport
                    .range
                    .pan_from(model.domain, drag.start_value, value),
            });
            let Some(viewport) = viewport else {
                return;
            };
            self.dispatch(Action::SetTimelineViewport(Some(viewport)), window, cx);
            return;
        }

        let Some((pointer_fraction, edge_fraction)) =
            self.timeline_drag_fractions(event.position.x)
        else {
            return;
        };
        let Some((axis, previous, viewport, current)) = self.with_timeline_model(|model| {
            let viewport = model.viewport.auto_pan(
                model.domain,
                pointer_fraction,
                edge_fraction,
                TIMELINE_EDGE_PAN_STEP_FRACTION,
            );
            (
                model.axis,
                model.viewport,
                viewport,
                viewport.value_at_fraction(pointer_fraction),
            )
        }) else {
            return;
        };
        if let Some(drag) = &mut self.timeline_drag {
            drag.current_value = current;
        }
        let viewport = (viewport != previous).then_some(AxisRange {
            axis,
            range: viewport,
        });
        if let Some(viewport) = viewport {
            self.dispatch(Action::SetTimelineViewport(Some(viewport)), window, cx);
        } else {
            cx.notify();
        }
    }

    pub(super) fn timeline_mouse_up(
        &mut self,
        event: &MouseUpEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(drag) = self.timeline_drag.take() else {
            return;
        };
        if self.with_timeline_model(|model| model.axis) != Some(drag.initial_viewport.axis) {
            cx.notify();
            return;
        }
        let moved = (f32::from(event.position.x) - drag.start_x).abs() >= TIMELINE_CLICK_SLOP;
        if drag.pan {
            let end = self
                .timeline_value_in_viewport(event.position.x, Some(drag.initial_viewport))
                .unwrap_or(drag.start_value);
            let viewport = self.with_timeline_model(|model| AxisRange {
                axis: model.axis,
                range: drag
                    .initial_viewport
                    .range
                    .pan_from(model.domain, drag.start_value, end),
            });
            if moved {
                if let Some(viewport) = viewport {
                    self.dispatch(Action::SetTimelineViewport(Some(viewport)), window, cx);
                }
            } else {
                self.dispatch(Action::SetTimelineSelection(None), window, cx);
            }
            cx.notify();
            return;
        }
        let Some(end) = self.timeline_value(event.position.x) else {
            self.cancel_timeline_gesture();
            return;
        };
        if !moved
            && let Some(record_id) = drag.record_id.as_ref()
            && let Some(index) = self.timeline_index_for_id(record_id)
        {
            self.cancel_timeline_gesture();
            self.select_trajectory(index, TrajectorySelectionSource::Timeline, window, cx);
            return;
        }
        let Some((axis, domain, viewport)) =
            self.with_timeline_model(|model| (model.axis, model.domain, model.viewport))
        else {
            self.cancel_timeline_gesture();
            return;
        };
        let span_count = self
            .with_timeline_geometry(|geometry| geometry.cells.len())
            .unwrap_or_default();
        let minimum = minimum_timeline_selection_width(domain, viewport, span_count);
        let selection = AxisRange {
            axis,
            range: DomainRange::new(drag.start_value, end).with_minimum_width(domain, minimum),
        };
        self.dispatch(Action::SetTimelineSelection(Some(selection)), window, cx);
        self.scroll_trajectory_range_into_view(selection, cx);
        if !moved
            && drag.record_id.is_none()
            && let Some(index) = self.nearest_timeline_record(event.position)
            && let Some(record) = self.core.session_view.trajectory.records.get(index)
        {
            self.dispatch(
                Action::SelectDetails(Some(DetailsSelection::Record(record.id.clone()))),
                window,
                cx,
            );
            self.details_scroll
                .set_offset(gpui_kit::point(px(0.0), px(0.0)));
            self.scroll_trajectory_to_record(index, cx);
        }
        cx.notify();
    }

    pub(super) fn nearest_timeline_record(&self, position: Point<Pixels>) -> Option<usize> {
        let bounds = self.timeline_bounds?;
        let x = f64::from(f32::from(position.x - bounds.origin.x));
        let lane = timeline_lane_at(f32::from(position.y - bounds.origin.y));
        self.with_timeline_model(|model| {
            let nearest = |same_lane: bool| {
                model
                    .cells
                    .iter()
                    .filter(|cell| !same_lane || lane == Some(cell.lane))
                    .filter_map(|cell| {
                        let distance = if x < cell.start_px {
                            cell.start_px - x
                        } else if x > cell.end_px {
                            x - cell.end_px
                        } else {
                            0.0
                        };
                        cell.ids.last().copied().map(|index| (distance, index))
                    })
                    .min_by(|left, right| left.0.total_cmp(&right.0))
                    .map(|(_, index)| index)
            };
            nearest(true).or_else(|| nearest(false))
        })
        .flatten()
    }

    pub(super) fn timeline_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .with_timeline_model(|model| model.cells.is_empty())
            .unwrap_or(true)
        {
            return;
        }
        let Some(anchor) = self.timeline_value(event.position.x) else {
            return;
        };
        let delta = event.delta.pixel_delta(window.line_height()).y;
        if delta == px(0.0) {
            return;
        }
        let factor = if delta > px(0.0) { 1.25 } else { 0.8 };
        let minimum_width = self
            .with_timeline_model(|model| match self.core.trajectory.mode {
                TimelineMode::Sequence => 4.0_f64.min(model.domain.width()),
                TimelineMode::Duration | TimelineMode::Actual => 20.0_f64.min(model.domain.width()),
            })
            .unwrap_or(f64::EPSILON);
        let viewport = self.with_timeline_model(|model| AxisRange {
            axis: model.axis,
            range: model
                .viewport
                .zoom(model.domain, anchor, factor, minimum_width),
        });
        let Some(viewport) = viewport else {
            return;
        };
        self.dispatch(Action::SetTimelineViewport(Some(viewport)), window, cx);
        cx.stop_propagation();
    }

    pub(super) fn timeline_value(&self, x: gpui_kit::Pixels) -> Option<f64> {
        self.timeline_value_in_viewport(x, None)
    }

    pub(super) fn timeline_drag_fractions(&self, x: gpui_kit::Pixels) -> Option<(f64, f64)> {
        let bounds = self.timeline_bounds?;
        let width = f64::from(f32::from(bounds.size.width));
        if !width.is_finite() || width <= 0.0 {
            return None;
        }
        let local_x = f64::from(f32::from(x - bounds.origin.x));
        let pointer_fraction = (local_x / width).clamp(0.0, 1.0);
        let edge_px =
            (width * TIMELINE_EDGE_PAN_ZONE_FRACTION).clamp(1.0, TIMELINE_EDGE_PAN_MAX_PX);
        Some((pointer_fraction, (edge_px / width).clamp(0.0, 0.5)))
    }

    pub(super) fn timeline_value_in_viewport(
        &self,
        x: gpui_kit::Pixels,
        viewport: Option<AxisRange>,
    ) -> Option<f64> {
        let bounds = self.timeline_bounds?;
        self.ensure_timeline_model_cache();
        let cache = self.timeline_model_cache.borrow();
        let cache = cache.as_ref()?;
        let model = cache.model.as_ref()?;
        let viewport = viewport
            .filter(|range| range.axis == model.axis)
            .map_or(model.viewport, |range| range.range);
        let local_x = f64::from(f32::from(x - bounds.origin.x));
        let width = f64::from(f32::from(bounds.size.width));
        Some(viewport.value_at_fraction(local_x / width.max(1.0)))
    }

    pub(super) fn update_timeline_hover(
        &mut self,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let Some(bounds) = self.timeline_bounds else {
            return;
        };
        let Some((axis, empty)) =
            self.with_timeline_model(|model| (model.axis, model.cells.is_empty()))
        else {
            return;
        };
        if empty {
            if self.timeline_hover.take().is_some() {
                cx.notify();
            }
            return;
        }
        let fraction =
            f64::from(((position.x - bounds.origin.x) / bounds.size.width).clamp(0.0, 1.0));
        let record_id = self.timeline_record_id(position);
        let hover = Some(TimelineHoverState {
            axis,
            fraction,
            record_id,
        });
        if self.timeline_hover != hover {
            self.timeline_hover = hover;
            cx.notify();
        }
    }

    pub(super) fn timeline_record_id(&self, position: Point<Pixels>) -> Option<TrajectoryItemId> {
        let bounds = self.timeline_bounds?;
        let local_y = f32::from(position.y - bounds.origin.y);
        let lane = timeline_lane_at(local_y)?;
        let fraction =
            f64::from(((position.x - bounds.origin.x) / bounds.size.width).clamp(0.0, 1.0));
        self.ensure_timeline_model_cache();
        let cache = self.timeline_model_cache.borrow();
        let cache = cache.as_ref()?;
        let index = cache.model.as_ref()?.hit_test(lane, fraction)?;
        self.core
            .session_view
            .trajectory
            .records
            .get(index)
            .map(|record| record.id.clone())
    }

    pub(super) fn timeline_index_for_id(&self, id: &TrajectoryItemId) -> Option<usize> {
        self.core.session_view.trajectory.record_index(id)
    }

    pub(crate) fn cancel_timeline_gesture(&mut self) {
        self.timeline_drag = None;
    }
}
