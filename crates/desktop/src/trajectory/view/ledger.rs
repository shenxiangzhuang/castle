use super::*;

impl DesktopApp {
    pub(super) fn sync_trajectory_ledger_list(&self, rows: &TimelineRows, query_active: bool) {
        let structure = (
            self.core.session_view.trajectory.projection_lineage(),
            if query_active {
                0
            } else {
                self.core.trajectory.fold_revision
            },
            if query_active {
                0
            } else {
                self.core
                    .session_view
                    .trajectory
                    .request_boundary_revision()
            },
            query_active,
        );
        let structure_changed =
            self.trajectory_list_structure.replace(Some(structure)) != Some(structure);
        sync_trajectory_list_state(
            &self.trajectory_scroll,
            rows.len(),
            structure_changed,
            self.trajectory_scroll_restore.take(),
            self.trajectory_follow_tail.get(),
        );
    }

    #[allow(
        clippy::expect_used,
        reason = "ledger list state is synchronized with the same projected rows"
    )]
    pub(super) fn trajectory_ledger(
        &self,
        rows: &TimelineRows,
        query_active: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        self.sync_trajectory_ledger_list(rows, query_active);
        let rows = rows.clone();
        let compact = self.trajectory_ledger_is_compact();
        let entity = cx.entity().clone();
        let layout_generation = self.core.layout_generation;
        self.ensure_timeline_model_cache();
        let focus = self
            .timeline_model_cache
            .borrow()
            .as_ref()
            .and_then(|cache| cache.focus.as_ref())
            .cloned();
        let list = gpui_kit::list(
            self.trajectory_scroll.clone(),
            cx.processor(move |this, row_index: usize, _, cx| {
                let row = rows
                    .get(row_index)
                    .expect("trajectory list state must match the projected ledger rows");
                match row {
                    TimelineLedgerRow::Record(index) => this.trajectory_row(
                        index,
                        row_index,
                        &rows,
                        focus.as_ref().map(|focus| focus.record_indices.as_ref()),
                        compact,
                        cx,
                    ),
                    TimelineLedgerRow::RequestBoundary {
                        request,
                        run_index,
                        terminal,
                    } => this.trajectory_pending_request_row(
                        request,
                        run_index,
                        terminal,
                        focus.is_some(),
                        compact,
                        cx,
                    ),
                    TimelineLedgerRow::TurnSummary {
                        representative,
                        turn,
                        first_hidden,
                        last_hidden,
                        step_ids,
                        call_count,
                    } => this.trajectory_turn_summary(
                        representative,
                        turn,
                        first_hidden,
                        last_hidden,
                        step_ids.len(),
                        call_count,
                        focus.as_ref(),
                        compact,
                        cx,
                    ),
                    TimelineLedgerRow::CallsSummary {
                        assistant,
                        first_tool,
                        last_tool,
                        tools,
                        ..
                    } => this.trajectory_calls_summary(
                        assistant,
                        first_tool,
                        last_tool,
                        tools,
                        focus.as_ref(),
                        compact,
                        cx,
                    ),
                }
            }),
        )
        .size_full();
        div()
            .id("trajectory-ledger-v1")
            .size_full()
            .child(list)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    this.dispatch(Action::SelectDetails(None), window, cx);
                    this.dispatch(Action::SetTimelineSelection(None), window, cx);
                }),
            )
            .on_prepaint(move |bounds, _, cx| {
                entity.update(cx, |this, cx| {
                    let previous = this.trajectory_ledger_is_compact();
                    this.trajectory_ledger_width = Some((layout_generation, bounds.size.width));
                    if this.trajectory_ledger_is_compact() != previous {
                        cx.notify();
                    }
                });
            })
    }

    pub(super) fn trajectory_ledger_is_compact(&self) -> bool {
        let measured = self
            .trajectory_ledger_width
            .filter(|(generation, _)| *generation == self.core.layout_generation)
            .map(|(_, width)| f32::from(width));
        let width = measured.unwrap_or_else(|| match self.core.layout.trajectory {
            TrajectoryMode::Split => (self.core.layout.main_width
                - trajectory_details_default_width(self.core.layout.main_width))
            .max(0.0),
            TrajectoryMode::Ledger | TrajectoryMode::Overlay => self.core.layout.main_width,
        });
        width <= COMPACT_LEDGER_MAX_WIDTH
    }

    pub(super) fn trajectory_row(
        &self,
        index: usize,
        ledger_row: usize,
        rows: &TimelineRows,
        focused: Option<&HashSet<usize>>,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let colors = trajectory_palette(cx);
        let record = &self.core.session_view.trajectory.records[index];
        let selected = self
            .core
            .details
            .selected
            .as_ref()
            .and_then(DetailsSelection::record)
            == Some(&record.id);
        let active_turn =
            self.core
                .details
                .selected
                .as_ref()
                .and_then(|selection| match selection {
                    DetailsSelection::Record(id) => self
                        .core
                        .session_view
                        .trajectory
                        .record_by_id(id)
                        .and_then(|record| record.turn),
                    DetailsSelection::Request(key) => self
                        .core
                        .session_view
                        .trajectory
                        .request_by_key(key)
                        .and_then(|request| request.turn),
                });
        let active_turn = record.turn.is_some() && record.turn == active_turn;
        let outside = focused.is_some_and(|focused| !focused.contains(&index));
        let opacity = if outside { 0.24 } else { 1.0 };
        let kind_color = record_color(record, colors);
        let marker_hovered = self.request_marker_hover.as_ref().is_some_and(|hovered| {
            self.core
                .session_view
                .trajectory
                .requests_for_boundary(&record.id)
                .any(|request| &request.key == hovered)
        });
        let request_markers = self
            .core
            .session_view
            .trajectory
            .requests_for_boundary(&record.id)
            .enumerate()
            .map(|(run_index, request)| {
                self.trajectory_request_marker(
                    request,
                    outside,
                    opacity,
                    compact,
                    u16::try_from(run_index).unwrap_or(u16::MAX),
                    cx,
                )
            })
            .collect::<Vec<_>>();
        let (turn_start, _, _) = ledger_record_boundaries(
            rows,
            ledger_row,
            record,
            &self.core.session_view.trajectory.records,
        );
        div()
            .id(("trajectory-record-v1", record.source_seq))
            .relative()
            .flex()
            .items_center()
            .w_full()
            .h(px(metrics::LEDGER_ROW_HEIGHT))
            .pr_3()
            .border_b_1()
            .border_color(colors.border_l1.opacity(opacity))
            .when(selected, |row| row.bg(colors.primary.opacity(0.035)))
            .when(!marker_hovered, |row| row.hover(|row| row.bg(colors.hover)))
            .cursor_pointer()
            .tab_index(0)
            .child(trajectory_event_cell(
                record,
                turn_start,
                compact,
                outside,
                opacity,
                kind_color,
                colors,
                active_turn,
                selected,
            ))
            .children(request_markers)
            .child(trajectory_record_preview(
                record,
                colors,
                opacity,
                self.trajectory_ledger_width
                    .filter(|(generation, _)| *generation == self.core.layout_generation)
                    .map(|(_, width)| tool_request_column_width(f32::from(width), compact)),
            ))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    // Row gestures must not bubble into the ledger's blank-area deselection.
                    cx.stop_propagation();
                    if event.click_count >= 2 {
                        let collapsible_turns =
                            this.core.session_view.trajectory.collapsible_turns();
                        let collapsible_assistants =
                            this.core.session_view.trajectory.collapsible_assistants();
                        let Some(target) = ledger_double_click_target(
                            &this.core.session_view.trajectory.records[index],
                            turn_start,
                            &this.core.trajectory.collapsed_turns,
                            &collapsible_turns,
                            &collapsible_assistants,
                        ) else {
                            return;
                        };
                        let action = match target {
                            LedgerFoldTarget::Turn(turn) => Action::ToggleTrajectoryTurn(turn),
                            LedgerFoldTarget::Assistant(assistant) => {
                                Action::ToggleTrajectoryAssistant(assistant)
                            }
                        };
                        this.dispatch(action, window, cx);
                        return;
                    }
                    if event.click_count == 1 {
                        this.select_trajectory(
                            index,
                            TrajectorySelectionSource::Ledger,
                            window,
                            cx,
                        );
                    }
                }),
            )
            .on_key_down(
                cx.listener(move |this, event: &gpui_kit::KeyDownEvent, window, cx| {
                    if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                        this.select_trajectory(
                            index,
                            TrajectorySelectionSource::Ledger,
                            window,
                            cx,
                        );
                    }
                }),
            )
            .into_any_element()
    }

    pub(super) fn trajectory_request_marker(
        &self,
        request: &TrajectoryRequest,
        outside: bool,
        opacity: f32,
        compact: bool,
        run_index: u16,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let colors = trajectory_palette(cx);
        let selected = self
            .core
            .details
            .selected
            .as_ref()
            .and_then(DetailsSelection::request)
            == Some(&request.key);
        let number = request.number;
        let key = request.key.clone();
        let keyboard_key = key.clone();
        let hover_key = key.clone();
        let error = matches!(
            request.status,
            ItemStatus::Failed | ItemStatus::Aborted | ItemStatus::Unknown
        );
        let default_color = if error {
            colors.error
        } else {
            colors.label_caption
        }
        .opacity(if outside { 0.18 } else { opacity });
        let hover_color = if error { colors.error } else { colors.primary };
        let marker_label = if request.purpose == TrajectoryRequestPurpose::Compaction {
            format!("Request #{number} · Compaction")
        } else {
            format!("Request #{number}")
        };
        let hover_label = marker_label.clone();
        let group = SharedString::from(format!("trajectory-request-marker-{number}"));
        div()
            .id(("trajectory-request-marker", number))
            .absolute()
            .left(px(
                (if compact { 6.0 } else { 12.0 }) + f32::from(run_index) * 8.0
            ))
            .top(px(-8.0))
            .size(px(16.0))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .tab_index(0)
            .role(Role::Button)
            .aria_label(marker_label)
            .group(group.clone())
            .child(
                div()
                    .size(px(if selected { 8.0 } else { 5.0 }))
                    .rounded_full()
                    .when(selected, |dot| {
                        dot.border_1().border_color(colors.primary).bg(if error {
                            colors.error
                        } else {
                            colors.primary.opacity(0.18)
                        })
                    })
                    .when(!selected, |dot| {
                        dot.bg(default_color)
                            .group_hover(group.clone(), move |dot| dot.bg(hover_color))
                    }),
            )
            .child(
                div()
                    .absolute()
                    .top(px(2.0))
                    .left(px(17.0))
                    .invisible()
                    .group_hover(group, |label| label.visible())
                    .px_1()
                    .h(px(14.0))
                    .flex()
                    .items_center()
                    .rounded(px(2.0))
                    .border_1()
                    .border_color(colors.border_l1)
                    .bg(colors.background)
                    .shadow_sm()
                    .text_size(px(9.0))
                    .text_color(colors.label_secondary)
                    .whitespace_nowrap()
                    .child(hover_label),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _: &MouseDownEvent, _, cx| cx.stop_propagation()),
            )
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                let changed = if *hovered {
                    if this.request_marker_hover.as_ref() == Some(&hover_key) {
                        false
                    } else {
                        this.request_marker_hover = Some(hover_key.clone());
                        true
                    }
                } else if this.request_marker_hover.as_ref() == Some(&hover_key) {
                    this.request_marker_hover = None;
                    true
                } else {
                    false
                };
                if changed {
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.dispatch(
                    Action::SelectDetails(Some(DetailsSelection::Request(key.clone()))),
                    window,
                    cx,
                );
                this.dispatch(Action::SetDetailsTab(DetailsTab::Summary), window, cx);
                this.details_scroll
                    .set_offset(gpui_kit::point(px(0.0), px(0.0)));
            }))
            .on_key_down(
                cx.listener(move |this, event: &gpui_kit::KeyDownEvent, window, cx| {
                    if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                        this.dispatch(
                            Action::SelectDetails(Some(DetailsSelection::Request(
                                keyboard_key.clone(),
                            ))),
                            window,
                            cx,
                        );
                        this.dispatch(Action::SetDetailsTab(DetailsTab::Summary), window, cx);
                        this.details_scroll
                            .set_offset(gpui_kit::point(px(0.0), px(0.0)));
                        cx.stop_propagation();
                    }
                }),
            )
            .into_any_element()
    }

    pub(super) fn trajectory_pending_request_row(
        &self,
        request_index: usize,
        run_index: u16,
        terminal: bool,
        outside_focus: bool,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let request = &self.core.session_view.trajectory.requests[request_index];
        let opacity = if outside_focus { 0.24 } else { 1.0 };
        let marker =
            self.trajectory_request_marker(request, outside_focus, opacity, compact, run_index, cx);
        div()
            .id(("trajectory-pending-request", request.number))
            .relative()
            .w_full()
            .h(px(if terminal { 9.0 } else { 0.0 }))
            .child(marker)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _: &MouseDownEvent, _, cx| cx.stop_propagation()),
            )
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn trajectory_turn_summary(
        &self,
        representative: usize,
        turn: u32,
        first_hidden: usize,
        last_hidden: usize,
        step_count: usize,
        call_count: usize,
        focus: Option<&TimelineFocusCache>,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let colors = trajectory_palette(cx);
        let outside =
            focus.is_some_and(|focus| !focus.intersects_non_system(first_hidden, last_hidden));
        let opacity = if outside { 0.24 } else { 1.0 };
        div()
            .id(("trajectory-turn-summary-v1", representative))
            .flex()
            .items_center()
            .w_full()
            .h(px(LEDGER_SUMMARY_ROW_HEIGHT))
            .pr_3()
            .border_b_1()
            .border_color(colors.border_l1.opacity(opacity))
            .hover(|row| row.bg(colors.hover))
            .cursor_pointer()
            .tab_index(0)
            .child(
                div()
                    .flex_none()
                    .w(px(if compact { 50.0 } else { 122.0 }))
                    .h_full(),
            )
            .child(
                div()
                    .flex_1()
                    .pl_2()
                    .min_w(px(0.0))
                    .truncate()
                    .text_xs()
                    .text_color(colors.label_tertiary.opacity(opacity))
                    .child(turn_summary_text(step_count, call_count)),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _: &MouseDownEvent, _, cx| cx.stop_propagation()),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.dispatch(Action::ToggleTrajectoryTurn(turn), window, cx);
            }))
            .on_key_down(
                cx.listener(move |this, event: &gpui_kit::KeyDownEvent, window, cx| {
                    if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                        this.dispatch(Action::ToggleTrajectoryTurn(turn), window, cx);
                    }
                }),
            )
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn trajectory_calls_summary(
        &self,
        assistant: usize,
        first_tool: usize,
        last_tool: usize,
        tools: Arc<str>,
        focus: Option<&TimelineFocusCache>,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let colors = trajectory_palette(cx);
        let outside = focus.is_some_and(|focus| !focus.intersects(first_tool, last_tool));
        let opacity = if outside { 0.24 } else { 1.0 };
        let count = last_tool - first_tool + 1;
        let assistant_id = self.core.session_view.trajectory.records[assistant]
            .id
            .clone();
        let assistant_key_id = assistant_id.clone();
        div()
            .id(("trajectory-calls-summary-v1", assistant))
            .flex()
            .items_center()
            .w_full()
            .h(px(LEDGER_SUMMARY_ROW_HEIGHT))
            .pr_3()
            .border_b_1()
            .border_color(colors.border_l1.opacity(opacity))
            .hover(|row| row.bg(colors.hover))
            .cursor_pointer()
            .tab_index(0)
            .child(
                div()
                    .flex_none()
                    .w(px(if compact { 50.0 } else { 122.0 }))
                    .h_full(),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .pl_2()
                    .truncate()
                    .text_xs()
                    .text_color(colors.label_tertiary.opacity(opacity))
                    .child(calls_summary_text(count, &tools)),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _: &MouseDownEvent, _, cx| cx.stop_propagation()),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.dispatch(
                    Action::ToggleTrajectoryAssistant(assistant_id.clone()),
                    window,
                    cx,
                );
            }))
            .on_key_down(
                cx.listener(move |this, event: &gpui_kit::KeyDownEvent, window, cx| {
                    if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                        this.dispatch(
                            Action::ToggleTrajectoryAssistant(assistant_key_id.clone()),
                            window,
                            cx,
                        );
                    }
                }),
            )
            .into_any_element()
    }
}
