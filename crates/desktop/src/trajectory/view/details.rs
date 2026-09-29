use super::*;

impl DesktopApp {
    pub(super) fn trajectory_details(
        &self,
        target: InspectorTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let colors = trajectory_palette(cx);
        let trajectory = &self.core.session_view.trajectory;
        let (tabs, title, location, status, kind_color, request_header) = match target {
            InspectorTarget::Record(index) => {
                let record = &trajectory.records[index];
                (
                    relevant_record_tabs(record, trajectory.record_details(&record.id)),
                    kind_label(record.kind).to_uppercase(),
                    record_location(record),
                    status_label(record.status),
                    record_color(record, colors),
                    false,
                )
            }
            InspectorTarget::Request(index) => {
                let request = &trajectory.requests[index];
                (
                    relevant_request_tabs(request),
                    format!("Request #{}", request.number),
                    if request.purpose == TrajectoryRequestPurpose::Compaction {
                        format!("Compaction · {}", request_location(request))
                    } else {
                        request_location(request)
                    },
                    status_label(request.status),
                    if matches!(
                        request.status,
                        ItemStatus::Failed | ItemStatus::Aborted | ItemStatus::Unknown
                    ) {
                        colors.error
                    } else {
                        colors.label_secondary
                    },
                    true,
                )
            }
        };
        let available = tabs
            .iter()
            .map(|descriptor| descriptor.tab)
            .collect::<Vec<_>>();
        let active = self.core.details.active_tab(&available);
        let identity = if request_header {
            div()
                .flex()
                .flex_1()
                .min_w(px(0.0))
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_none()
                        .size(px(5.0))
                        .rounded_full()
                        .bg(kind_color),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(px(12.0))
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .child(title),
                )
                .child(
                    div()
                        .min_w(px(0.0))
                        .truncate()
                        .text_size(px(11.0))
                        .text_color(colors.label_tertiary)
                        .child(location),
                )
                .into_any_element()
        } else {
            div()
                .flex()
                .flex_1()
                .min_w(px(0.0))
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_none()
                        .px_2()
                        .h(px(19.0))
                        .flex()
                        .items_center()
                        .rounded(px(4.0))
                        .text_size(px(10.0))
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(kind_color)
                        .bg(kind_color.opacity(0.1))
                        .child(title),
                )
                .child(
                    div()
                        .min_w(px(0.0))
                        .truncate()
                        .text_xs()
                        .text_color(colors.label_tertiary)
                        .child(format!("{} · {}", location, status)),
                )
                .into_any_element()
        };
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(colors.background)
            .border_l_1()
            .border_color(colors.border_l1)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(px(metrics::DETAILS_HEADER_HEIGHT))
                    .pl_3()
                    .pr_2()
                    .border_b_1()
                    .border_color(colors.border_l2)
                    .child(identity)
                    .child(
                        Button::new("close-trajectory-v1-details")
                            .icon(IconName::Close)
                            .with_size(px(28.0))
                            .ghost()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.dispatch(Action::SelectDetails(None), window, cx);
                            })),
                    ),
            )
            .child(
                div()
                    .id("trajectory-detail-tabs-v1")
                    .flex()
                    .items_center()
                    .h(px(34.0))
                    .px_2()
                    .gap(px(1.0))
                    .overflow_x_scroll()
                    .border_b_1()
                    .border_color(colors.border_l2)
                    .children(tabs.into_iter().enumerate().map(|(index, descriptor)| {
                        let tab = descriptor.tab;
                        let selected = tab == active;
                        div()
                            .id(("trajectory-detail-tab-v1", index))
                            .relative()
                            .flex()
                            .flex_none()
                            .items_center()
                            .h_full()
                            .px_2()
                            .cursor_pointer()
                            .text_sm()
                            .text_color(if tab == active {
                                colors.primary
                            } else {
                                colors.label_tertiary
                            })
                            .hover(move |element| element.bg(colors.hover))
                            .child(descriptor.label)
                            .children(selected.then(|| {
                                div()
                                    .absolute()
                                    .left(px(9.0))
                                    .right(px(9.0))
                                    .bottom_0()
                                    .h(px(2.0))
                                    .rounded_t(px(1.0))
                                    .bg(colors.primary)
                            }))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.dispatch(Action::SetDetailsTab(tab), window, cx);
                            }))
                    })),
            )
            .child(
                div()
                    .id("trajectory-details-v1-scroll")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.0))
                    .p_4()
                    .gap_4()
                    .track_scroll(&self.details_scroll)
                    .overflow_y_scrollbar()
                    .child(self.trajectory_details_body(target, active, window, cx)),
            )
            .into_any_element()
    }

    pub(super) fn trajectory_details_body(
        &self,
        target: InspectorTarget,
        tab: DetailsTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        match target {
            InspectorTarget::Record(index) => {
                self.trajectory_record_details_body(index, tab, window, cx)
            }
            InspectorTarget::Request(index) => self.trajectory_request_details_body(index, tab, cx),
        }
    }

    pub(super) fn trajectory_record_details_body(
        &self,
        index: usize,
        tab: DetailsTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let colors = trajectory_palette(cx);
        let trajectory = &self.core.session_view.trajectory;
        let record = &trajectory.records[index];
        let details = trajectory.record_details(&record.id);
        match tab {
            DetailsTab::Timing => self.timing_details(record, colors, cx),
            DetailsTab::SystemPrompt => prompt_snapshot(details).map_or_else(
                || empty_detail("No system prompt in this request", colors),
                |prompt| {
                    self.trajectory_markdown(
                        record,
                        TrajectoryMarkdownSource::SystemPrompt,
                        prompt.instructions(),
                        window,
                        cx,
                    )
                },
            ),
            DetailsTab::Preview => self.trajectory_markdown(
                record,
                TrajectoryMarkdownSource::Preview,
                &record.text,
                window,
                cx,
            ),
            DetailsTab::Payload => {
                code_panel(record.payload.as_deref().unwrap_or_default(), colors)
            }
            DetailsTab::Result => code_panel(&record.text, colors),
            DetailsTab::Raw => code_panel(&record.text, colors),
            DetailsTab::Diff => prompt_diff_panel(details, colors),
            DetailsTab::Tools => prompt_snapshot(details).map_or_else(
                || empty_detail("No tools in this request", colors),
                |prompt| prompt_tools_panel(prompt, colors),
            ),
            DetailsTab::Schema => details
                .and_then(TrajectoryRecordDetails::tool_schema)
                .map_or_else(
                    || empty_detail("Tool schema unavailable", colors),
                    |schema| code_panel(schema, colors),
                ),
            DetailsTab::Options => empty_detail("Options belong to the request", colors),
            DetailsTab::Usage => record.usage.map_or_else(
                || empty_detail("Usage unavailable", colors),
                |usage| {
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(detail_pair(
                            "Input tokens",
                            &usage.input_tokens().to_string(),
                            colors,
                        ))
                        .child(detail_pair(
                            "Output tokens",
                            &usage.total_output_tokens().to_string(),
                            colors,
                        ))
                        .child(detail_pair(
                            "Cached tokens",
                            &usage.cache_read_input_tokens.to_string(),
                            colors,
                        ))
                        .into_any_element()
                },
            ),
            DetailsTab::Summary => {
                let request = match details {
                    Some(TrajectoryRecordDetails::Tool { request_key, .. }) => {
                        trajectory.request_by_key(request_key)
                    }
                    _ => trajectory.request_for_record(&record.id),
                };
                let request_link = request.map(|request| {
                    let key = request.key.clone();
                    div()
                        .id(("trajectory-source-request", request.number))
                        .flex()
                        .justify_between()
                        .gap_4()
                        .text_sm()
                        .cursor_pointer()
                        .child(div().text_color(colors.label_tertiary).child("Request"))
                        .child(
                            div()
                                .text_color(colors.primary)
                                .child(format!("Request #{}", request.number)),
                        )
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
                });
                let assistant_link = matches!(record.kind, TrajectoryKind::Tool)
                    .then(|| request.and_then(|request| request.result.as_ref()))
                    .flatten()
                    .and_then(|id| trajectory.record_index(id))
                    .map(|assistant_index| {
                        let label = trajectory.records[assistant_index].title.clone();
                        div()
                            .id(("trajectory-tool-assistant", record.source_seq))
                            .flex()
                            .justify_between()
                            .gap_4()
                            .text_sm()
                            .cursor_pointer()
                            .child(div().text_color(colors.label_tertiary).child("Assistant"))
                            .child(div().truncate().text_color(colors.primary).child(label))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.reveal_and_select_trajectory(assistant_index, window, cx);
                            }))
                    });
                let parent_link = match details {
                    Some(TrajectoryRecordDetails::Tool {
                        parent_call_id: Some(parent),
                        ..
                    }) => trajectory.record_index(&TrajectoryItemId::Tool(parent.clone())),
                    _ => None,
                }
                .map(|parent_index| {
                    let label = trajectory.records[parent_index].title.clone();
                    div()
                        .id(("trajectory-parent-tool", record.source_seq))
                        .flex()
                        .justify_between()
                        .gap_4()
                        .text_sm()
                        .cursor_pointer()
                        .child(div().text_color(colors.label_tertiary).child("Parent tool"))
                        .child(div().truncate().text_color(colors.primary).child(label))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.reveal_and_select_trajectory(parent_index, window, cx);
                        }))
                });
                let request_timing = (record.kind == TrajectoryKind::Assistant)
                    .then_some(request)
                    .flatten()
                    .map(|request| {
                        let key = request.key.clone();
                        let value = timing_duration(record);
                        div()
                            .id(("trajectory-assistant-request-timing", request.number))
                            .mt_2()
                            .p_3()
                            .rounded(px(6.0))
                            .border_1()
                            .border_color(colors.border_l2)
                            .cursor_pointer()
                            .hover(|card| card.bg(colors.hover))
                            .child(
                                div()
                                    .flex()
                                    .justify_between()
                                    .gap_4()
                                    .text_sm()
                                    .child(
                                        div()
                                            .text_color(colors.label_secondary)
                                            .child("Request Timing"),
                                    )
                                    .child(value),
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.dispatch(
                                    Action::SelectDetails(Some(DetailsSelection::Request(
                                        key.clone(),
                                    ))),
                                    window,
                                    cx,
                                );
                                this.dispatch(
                                    Action::SetDetailsTab(DetailsTab::Timing),
                                    window,
                                    cx,
                                );
                                this.details_scroll
                                    .set_offset(gpui_kit::point(px(0.0), px(0.0)));
                            }))
                    });
                let mut body = div()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(detail_pair("Status", status_label(record.status), colors))
                    .children(request_link)
                    .children(assistant_link)
                    .children(parent_link)
                    .children(match &record.id {
                        TrajectoryItemId::Tool(call_id) => {
                            Some(detail_pair("Call ID", call_id.as_str(), colors))
                        }
                        _ => None,
                    })
                    .children(record.usage.map(|usage| usage_summary(usage, colors)))
                    .children(request_timing);
                if !record.text.trim().is_empty() {
                    body = body.child(code_panel(&record.text, colors));
                }
                body.into_any_element()
            }
        }
    }

    pub(super) fn trajectory_request_details_body(
        &self,
        index: usize,
        tab: DetailsTab,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let colors = trajectory_palette(cx);
        let request = &self.core.session_view.trajectory.requests[index];
        match tab {
            DetailsTab::Summary => {
                let result_link = request.result.as_ref().and_then(|id| {
                    let record_index = self.core.session_view.trajectory.record_index(id)?;
                    let label = self.core.session_view.trajectory.records[record_index]
                        .title
                        .clone();
                    Some(
                        div()
                            .id(("trajectory-request-result", request.number))
                            .flex()
                            .justify_between()
                            .gap_4()
                            .text_sm()
                            .cursor_pointer()
                            .child(div().text_color(colors.label_tertiary).child("Result"))
                            .child(div().truncate().text_color(colors.primary).child(label))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.reveal_and_select_trajectory(record_index, window, cx);
                                this.dispatch(
                                    Action::SetDetailsTab(DetailsTab::Summary),
                                    window,
                                    cx,
                                );
                            })),
                    )
                });
                let model = request.response_model.as_deref().or_else(|| {
                    request
                        .options
                        .as_deref()
                        .map(|options| options.model.as_ref())
                });
                let mut body = div().flex().flex_col().gap_3().child(detail_pair(
                    "Status",
                    status_label(request.status),
                    colors,
                ));
                if request.purpose == TrajectoryRequestPurpose::Compaction {
                    body = body.child(detail_pair(
                        "Purpose",
                        request_purpose_label(request.purpose),
                        colors,
                    ));
                }
                body = body
                    .child(detail_pair(
                        "Tool calls",
                        &request.tool_call_count.to_string(),
                        colors,
                    ))
                    .children((request.subtool_call_count > 0).then(|| {
                        detail_pair(
                            "Subtool calls",
                            &request.subtool_call_count.to_string(),
                            colors,
                        )
                    }))
                    .children(model.map(|model| detail_pair("Model", model, colors)))
                    .children(result_link)
                    .children(request.error.as_deref().map(|error| {
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(section_title("Error", colors))
                            .child(code_panel(error, colors))
                    }));
                if let Some(options) = request.options.as_ref() {
                    let summary = format!(
                        "{}{}",
                        options.model,
                        options
                            .reasoning_effort
                            .as_deref()
                            .map(|effort| format!(" · {effort}"))
                            .unwrap_or_default()
                    );
                    body = body.child(self.request_detail_preview(
                        request.number,
                        DetailsTab::Options,
                        "Options",
                        summary,
                        colors,
                        cx,
                    ));
                }
                let usage = request
                    .usage
                    .map(|usage| {
                        format!(
                            "{} input · {} output",
                            usage.input_tokens(),
                            usage.total_output_tokens()
                        )
                    })
                    .unwrap_or_else(|| "Unavailable".into());
                body.child(self.request_detail_preview(
                    request.number,
                    DetailsTab::Usage,
                    "Usage",
                    usage,
                    colors,
                    cx,
                ))
                .child(self.request_detail_preview(
                    request.number,
                    DetailsTab::Timing,
                    "Timing",
                    format_elapsed_duration(request.timing.duration_ns()),
                    colors,
                    cx,
                ))
                .into_any_element()
            }
            DetailsTab::Options => request.options.as_deref().map_or_else(
                || empty_detail("Options unavailable for this request", colors),
                |options| request_options_details(options, colors),
            ),
            DetailsTab::Usage => request_usage_details(request, colors),
            DetailsTab::Timing => self.request_timing_details(request, colors, cx),
            DetailsTab::SystemPrompt
            | DetailsTab::Diff
            | DetailsTab::Tools
            | DetailsTab::Preview
            | DetailsTab::Raw
            | DetailsTab::Payload
            | DetailsTab::Result
            | DetailsTab::Schema => empty_detail("This tab is not available for requests", colors),
        }
    }

    pub(super) fn request_detail_preview(
        &self,
        request_number: u32,
        tab: DetailsTab,
        title: &'static str,
        value: String,
        colors: TrajectoryPalette,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let id = SharedString::from(format!("request-{request_number}-{tab:?}-preview"));
        div()
            .id(id)
            .p_3()
            .rounded(px(6.0))
            .border_1()
            .border_color(colors.border_l2)
            .cursor_pointer()
            .hover(|card| card.bg(colors.hover))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .gap_4()
                    .text_sm()
                    .child(div().text_color(colors.label_secondary).child(title))
                    .child(div().truncate().child(value)),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.dispatch(Action::SetDetailsTab(tab), window, cx);
                this.details_scroll
                    .set_offset(gpui_kit::point(px(0.0), px(0.0)));
            }))
            .into_any_element()
    }

    pub(super) fn request_timing_details(
        &self,
        request: &TrajectoryRequest,
        colors: TrajectoryPalette,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        if let Some(record) = request
            .result
            .as_ref()
            .and_then(|id| self.core.session_view.trajectory.record_by_id(id))
            .filter(|record| record.kind == TrajectoryKind::Assistant)
        {
            return self.timing_details(record, colors, cx);
        }
        let anchor_started = request
            .anchor
            .as_ref()
            .and_then(|id| self.core.session_view.trajectory.record_by_id(id))
            .and_then(|record| record.timing.started.as_ref());
        let started_at = request.timing.started.as_ref().or(anchor_started);
        let started = started_at
            .map(|time| format_wall(time.wall_time_ms(), self.core.details.unix_time))
            .unwrap_or_else(|| "Not available".into());
        let started_value = if started_at.is_some() {
            div()
                .id(("toggle-request-timing-clock-format", request.number))
                .text_right()
                .cursor_pointer()
                .child(started)
                .on_click(cx.listener(|this, _, window, cx| {
                    this.dispatch(Action::ToggleDetailsUnixTime, window, cx);
                }))
                .into_any_element()
        } else {
            div().text_right().child(started).into_any_element()
        };
        let mut body = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .justify_between()
                    .gap_4()
                    .text_sm()
                    .child(div().text_color(colors.label_tertiary).child("Started"))
                    .child(started_value),
            )
            .child(detail_pair(
                "Duration",
                &format_elapsed_duration(request.timing.duration_ns()),
                colors,
            ));
        if request.timing.started.is_some() {
            body = body.child(detail_pair(
                "Timing source",
                if request.timing.completed.is_some() {
                    "Session timestamps"
                } else {
                    "Session timestamps (running)"
                },
                colors,
            ));
        }
        body.into_any_element()
    }

    pub(super) fn trajectory_markdown(
        &self,
        record: &Arc<TrajectoryRecord>,
        source_kind: TrajectoryMarkdownSource,
        source: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let generation = self.core.layout_generation;
        let panel_width = self
            .trajectory_details_layout
            .measured_details_width(generation)
            .unwrap_or_else(|| {
                self.trajectory_details_layout.details_width(
                    self.core.layout.trajectory,
                    generation,
                    self.core.layout.main_width,
                )
            });
        let mut cache = self.trajectory_details_markdown.borrow_mut();
        cache.sync(
            self.core.session_view.trajectory.projection_lineage(),
            &record.id,
            source_kind,
            source,
        );
        let selection = cache
            .selection
            .get_or_insert_with(|| crate::rendering::MessageSelection::new(window, cx))
            .frame(0);
        selection.clone().wrap(markdown::render_markdown(
            record.source_seq,
            &cache.markdown,
            false,
            (panel_width - 32.0).max(1.0),
            &selection,
            window,
            cx,
        ))
    }

    pub(super) fn timing_details(
        &self,
        record: &TrajectoryRecord,
        colors: TrajectoryPalette,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let started = format_started(record, self.core.details.unix_time);
        let started_value = if record.timing.started.is_some() {
            div()
                .id("toggle-timing-clock-format")
                .text_right()
                .cursor_pointer()
                .child(started)
                .on_click(cx.listener(|this, _, window, cx| {
                    this.dispatch(Action::ToggleDetailsUnixTime, window, cx);
                }))
                .into_any_element()
        } else {
            div().text_right().child(started).into_any_element()
        };
        let mut body = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .justify_between()
                    .gap_4()
                    .text_sm()
                    .child(div().text_color(colors.label_tertiary).child("Started"))
                    .child(started_value),
            )
            .child(detail_pair(
                if record.kind == TrajectoryKind::Assistant {
                    "Total duration"
                } else {
                    "Duration"
                },
                &timing_duration(record),
                colors,
            ));
        if record.kind != TrajectoryKind::Assistant {
            let timing_source = if record.timing.duration_ns().is_some() {
                "Session timestamps"
            } else if matches!(record.status, ItemStatus::Pending | ItemStatus::Running) {
                "Session timestamps (running)"
            } else {
                "Not available"
            };
            body = body.child(detail_pair("Timing source", timing_source, colors));
        }
        if record.kind == TrajectoryKind::Assistant {
            body = body
                .child(detail_pair("TTFT", &assistant_ttft(record), colors))
                .child(detail_pair(
                    "Generation",
                    &assistant_generation(record),
                    colors,
                ))
                .child(detail_pair(
                    "Throughput",
                    &assistant_throughput(record),
                    colors,
                ));
        }
        if record.kind == TrajectoryKind::Tool {
            body = body
                .child(section_title("Execution breakdown", colors))
                .child(detail_pair(
                    "Requested",
                    &format_timing_point(
                        record.timing.requested.as_ref(),
                        self.core.details.unix_time,
                        record,
                    ),
                    colors,
                ))
                .child(detail_pair(
                    "Authorization resolved",
                    &format_timing_point(
                        record.timing.authorization_resolved.as_ref(),
                        self.core.details.unix_time,
                        record,
                    ),
                    colors,
                ))
                .child(detail_pair(
                    "Dispatch intended",
                    &format_timing_point(
                        record.timing.dispatch_intended.as_ref(),
                        self.core.details.unix_time,
                        record,
                    ),
                    colors,
                ))
                .child(detail_pair(
                    "Execution started",
                    &format_timing_point(
                        record.timing.execution_started.as_ref(),
                        self.core.details.unix_time,
                        record,
                    ),
                    colors,
                ))
                .child(detail_pair(
                    "Request registration",
                    &format_elapsed_duration(record.timing.request_registration_ns()),
                    colors,
                ))
                .child(detail_pair(
                    "Authorization wait",
                    &format_elapsed_duration(record.timing.authorization_ns()),
                    colors,
                ))
                .child(detail_pair(
                    "Dispatch wait",
                    &format_elapsed_duration(record.timing.dispatch_ns()),
                    colors,
                ))
                .child(detail_pair(
                    "Runner start wait",
                    &format_elapsed_duration(record.timing.runner_start_ns()),
                    colors,
                ))
                .child(detail_pair(
                    "Execution duration",
                    &record
                        .timing
                        .execution_ns()
                        .map(|ns| format_elapsed_duration(Some(ns)))
                        .unwrap_or_else(|| execution_missing(record)),
                    colors,
                ))
                .child(detail_pair(
                    "Pre-execution",
                    &format_elapsed_duration(record.timing.pre_execution_ns()),
                    colors,
                ))
                .child(detail_pair(
                    "Post/commit wait",
                    &format_elapsed_duration(record.timing.post_execution_ns()),
                    colors,
                ))
                .child(detail_pair(
                    "Execution source",
                    "Monotonic execution timestamps",
                    colors,
                ));
        }
        body.into_any_element()
    }
}
