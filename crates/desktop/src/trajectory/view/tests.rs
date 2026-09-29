use std::collections::HashSet;
use std::sync::Arc;

use harness::{AssistantChunk, CallId, EventTime, RequestId, SessionEvent, TokenUsage};
use im::{HashSet as ImHashSet, Vector};

use crate::app::layout::TrajectoryMode;
use crate::session::document::SessionDocument;
use crate::session::document::tests::{fixture, recorded};
use crate::trajectory::timeline::{
    AxisId, AxisRange, DomainRange, RenderCell, RenderIdentity, TimelineLane,
};
use crate::{
    app::DetailsTab,
    session::{
        ItemStatus, LayoutGeneration, RecordTiming, TrajectoryItemId, TrajectoryKind,
        TrajectoryProjection, TrajectoryRecord,
    },
    trajectory::TimelineMode,
};

use super::{
    ScrollStrategy, TIMELINE_BAR_HEIGHT, TIMELINE_BAR_OFFSET, TimelineCacheIdentity,
    TimelineCellMatches, TimelineFocusCache, TimelineLedgerRow, TimelineMatches, TimelineModel,
    TimelineModelCache, TimelineRows, TimelineSearchCache, TimelineView,
    TrajectoryDetailsLayoutState, TrajectoryDetailsMarkdownCache, TrajectoryMarkdownSource,
    TrajectorySelectionSource, aligned_trajectory_list_offset, calls_summary_text,
    clamp_trajectory_details_width, focus_scroll_target, minimum_timeline_selection_width,
    nested_segment_geometry, normalized_range, record_tooltip, resolved_trajectory_details_width,
    should_clear_selection_for_record, sync_trajectory_list_state, timeline_bar_gap_px,
    timeline_block_opacity, timeline_geometry, timeline_lane_at, timeline_lane_top, timeline_model,
    timeline_turn_boundary_fractions, trajectory_ledger_row_height, turn_summary_text,
};

fn time(ms: u64) -> EventTime {
    EventTime {
        wall_time_ms: 1_000 + ms as i64,
        clock_id: "timeline-test".into(),
        monotonic_ns: ms * 1_000_000,
    }
}

fn record(id: u64, start: u64, end: u64) -> TrajectoryRecord {
    let timing = RecordTiming {
        started: Some((&time(start)).into()),
        requested: Some((&time(start + 5)).into()),
        authorization_resolved: Some((&time(start + 10)).into()),
        execution_started: Some((&time(start + 20)).into()),
        execution_finished: Some((&time(end - 20)).into()),
        completed: Some((&time(end)).into()),
        ..RecordTiming::default()
    };
    TrajectoryRecord {
        id: TrajectoryItemId::Tool(CallId::from_raw(format!("call-{id}"))),
        source_seq: id,
        kind: TrajectoryKind::Tool,
        title: "tool".into(),
        text: String::new(),
        payload: None,
        turn: Some(1),
        step: Some(1),
        status: ItemStatus::Completed,
        timing,
        usage: None,
        search_text: "tool\n".into(),
    }
}

#[test]
fn details_width_matches_dsh_default_explicit_and_overlay_rules() {
    assert_eq!(clamp_trajectory_details_width(100.0, 900.0), 320.0);
    assert_eq!(clamp_trajectory_details_width(800.0, 900.0), 620.0);
    assert_eq!(clamp_trajectory_details_width(500.4, 1_500.0), 500.0);

    assert_eq!(
        resolved_trajectory_details_width(TrajectoryMode::Split, 900.0, None),
        342.0
    );
    assert_eq!(
        resolved_trajectory_details_width(TrajectoryMode::Split, 1_500.0, None),
        440.0
    );
    assert_eq!(
        resolved_trajectory_details_width(TrajectoryMode::Split, 900.0, Some(700.0)),
        620.0
    );
    assert_eq!(
        resolved_trajectory_details_width(TrajectoryMode::Split, 1_500.0, Some(700.0)),
        700.0
    );
    assert_eq!(
        resolved_trajectory_details_width(TrajectoryMode::Overlay, 600.0, None),
        420.0
    );
    assert_eq!(
        resolved_trajectory_details_width(TrajectoryMode::Overlay, 400.0, Some(500.0)),
        368.0
    );
}

#[test]
fn details_layout_uses_current_generation_and_frozen_drag_geometry() {
    let generation = LayoutGeneration(3);
    let mut state = TrajectoryDetailsLayoutState::default();
    assert!(state.observe_split_width(generation, 900.0));
    assert!(!state.observe_split_width(generation, 900.2));
    assert_eq!(state.split_width(generation, 1_200.0), 900.0);
    assert_eq!(state.split_width(generation.next(), 1_200.0), 1_200.0);

    assert!(state.observe_details_width(generation, 400.0));
    assert_eq!(state.measured_details_width(generation), Some(400.0));
    assert_eq!(state.measured_details_width(generation.next()), None);

    state.begin_drag(100.0, 400.0, 900.0);
    assert!(state.drag_to(80.0));
    assert_eq!(
        state.details_width(TrajectoryMode::Split, generation, 1_200.0),
        420.0
    );
    // A live container measurement must not change the geometry frozen at pointer-down.
    assert!(state.observe_split_width(generation, 1_500.0));
    assert!(state.drag_to(-500.0));
    assert_eq!(
        state.details_width(TrajectoryMode::Split, generation, 1_200.0),
        620.0
    );
    assert!(state.end_drag());
    assert!(!state.end_drag());
}

#[test]
fn details_layout_keyboard_step_and_reset_preserve_dsh_semantics() {
    let generation = LayoutGeneration(1);
    let mut state = TrajectoryDetailsLayoutState::default();
    state.observe_split_width(generation, 1_000.0);
    assert!(state.step(16.0, 400.0, 1_000.0));
    assert_eq!(
        state.details_width(TrajectoryMode::Split, generation, 1_000.0),
        416.0
    );
    assert!(state.step(-16.0, 416.0, 1_000.0));
    assert_eq!(
        state.details_width(TrajectoryMode::Split, generation, 1_000.0),
        400.0
    );
    assert!(state.reset());
    assert_eq!(
        state.details_width(TrajectoryMode::Split, generation, 1_000.0),
        380.0
    );
    assert!(!state.reset());
}

#[test]
fn selected_details_markdown_cache_reparses_only_when_content_identity_changes() {
    let record = record(9, 0, 100);
    let mut cache = TrajectoryDetailsMarkdownCache::default();

    cache.sync(10, &record.id, TrajectoryMarkdownSource::Preview, "first");
    assert_eq!(cache.markdown.revision(), 1);
    cache.sync(10, &record.id, TrajectoryMarkdownSource::Preview, "first");
    assert_eq!(cache.markdown.revision(), 1);

    let mut layout = TrajectoryDetailsLayoutState::default();
    layout.observe_details_width(LayoutGeneration(1), 400.0);
    layout.step(16.0, 400.0, 1_000.0);
    cache.sync(10, &record.id, TrajectoryMarkdownSource::Preview, "first");
    assert_eq!(cache.markdown.revision(), 1);

    cache.sync(
        10,
        &record.id,
        TrajectoryMarkdownSource::Preview,
        "first\n\nsecond",
    );
    assert_eq!(cache.markdown.revision(), 2);

    // A tab or session identity switch owns a fresh parser even if the bytes are equal.
    cache.sync(
        10,
        &record.id,
        TrajectoryMarkdownSource::SystemPrompt,
        "first\n\nsecond",
    );
    assert_eq!(cache.markdown.revision(), 1);
    cache.sync(
        11,
        &record.id,
        TrajectoryMarkdownSource::SystemPrompt,
        "first\n\nsecond",
    );
    assert_eq!(cache.markdown.revision(), 1);
}

fn cache_identity(
    document_generation: u64,
    revision: u64,
    mode: TimelineMode,
) -> TimelineCacheIdentity {
    TimelineCacheIdentity {
        axis: AxisId {
            document_generation,
            geometry_revision: revision,
            mode,
        },
        change_revision: revision,
    }
}

#[test]
fn timeline_turn_boundaries_follow_turn_starts_in_the_visible_domain() {
    let mut system = record(1, 0, 100);
    system.kind = TrajectoryKind::System;
    system.turn = None;
    let mut first_turn = record(2, 100, 200);
    first_turn.turn = Some(1);
    let mut same_turn = record(3, 200, 300);
    same_turn.turn = Some(1);
    let mut second_turn = record(4, 300, 400);
    second_turn.turn = Some(2);
    let records = [system, first_turn, same_turn, second_turn]
        .into_iter()
        .map(Arc::new)
        .collect::<Vector<_>>();
    let axis = AxisId {
        document_generation: 1,
        geometry_revision: 1,
        mode: TimelineMode::Sequence,
    };
    let materialized = records.iter().cloned().collect::<Vec<_>>();
    let geometry = timeline_geometry(&materialized, axis).expect("timeline geometry");

    assert_eq!(
        timeline_turn_boundary_fractions(&records, &geometry, geometry.domain),
        vec![0.25, 0.75]
    );
    assert_eq!(
        timeline_turn_boundary_fractions(&records, &geometry, DomainRange::new(1.0, 4.0),),
        vec![0.0, 2.0 / 3.0]
    );
}

#[test]
fn timeline_paint_uses_dsh_focus_search_and_role_opacity_precedence() {
    assert_eq!(
        timeline_block_opacity(TrajectoryKind::User, true, true, false),
        0.78
    );
    assert_eq!(
        timeline_block_opacity(TrajectoryKind::Tool, true, true, false),
        1.0
    );
    assert_eq!(
        timeline_block_opacity(TrajectoryKind::User, false, true, false),
        0.2
    );
    assert_eq!(
        timeline_block_opacity(TrajectoryKind::User, false, true, true),
        1.0
    );
    assert_eq!(
        timeline_block_opacity(TrajectoryKind::User, true, false, true),
        0.14
    );
}

#[test]
fn minimum_selection_width_uses_the_full_domain_operation_width() {
    let domain = DomainRange::new(0.0, 100.0);
    assert_eq!(
        minimum_timeline_selection_width(domain, DomainRange::new(40.0, 50.0), 100),
        1.0
    );
    assert_eq!(
        minimum_timeline_selection_width(domain, DomainRange::new(40.0, 40.5), 100),
        0.5
    );
}

#[test]
fn timeline_cache_reuses_geometry_when_only_the_viewport_changes() {
    let projection_lineage = 17;
    let records = [record(1, 0, 100)]
        .into_iter()
        .map(std::sync::Arc::new)
        .collect::<im::Vector<_>>();
    let axis = AxisId {
        document_generation: projection_lineage,
        geometry_revision: 4,
        mode: TimelineMode::Duration,
    };
    let mut cache = TimelineModelCache::new(
        cache_identity(projection_lineage, 4, TimelineMode::Duration),
        &records,
        TimelineView {
            viewport: Some(AxisRange {
                axis,
                range: DomainRange::new(0.0, 100.0),
            }),
            selection: None,
            render_width_px: 1_500.0,
        },
        None,
    );
    assert!(cache.geometry_matches(axis));
    assert!(!cache.geometry_matches(AxisId {
        geometry_revision: 5,
        ..axis
    }));

    let geometry_before = cache.geometry.as_ref().unwrap().cells.clone();
    let viewport = AxisRange {
        axis,
        range: DomainRange::new(10.0, 60.0),
    };
    cache.sync_ranges(Some(viewport), 1_500.0);
    assert_eq!(cache.viewport, Some(viewport));
    assert_eq!(cache.geometry.as_ref().unwrap().cells, geometry_before);
    assert_eq!(
        cache.model.as_ref().unwrap().viewport,
        DomainRange::new(10.0, 60.0)
    );
}

#[test]
fn hundred_thousand_row_stream_delta_touches_only_changed_search_record() {
    let mut records = (0..100_000)
        .map(|index| Arc::new(record(index, index, index.saturating_add(100))))
        .collect::<im::Vector<_>>();

    let model_cache = TimelineModelCache::new(
        cache_identity(77, 1, TimelineMode::Sequence),
        &records,
        TimelineView {
            viewport: None,
            selection: None,
            render_width_px: 1_500.0,
        },
        None,
    );
    let model = model_cache.model.as_ref().unwrap();
    let geometry = model_cache.geometry.as_ref().unwrap();
    assert!(model.cells.len() <= super::TIMELINE_PRIMITIVE_LIMIT);
    assert_eq!(
        model.cells.iter().map(|cell| cell.ids.len()).sum::<usize>(),
        100_000
    );
    assert!(model.cells.iter().all(|cell| matches!(
        cell.lane,
        TimelineLane::Input | TimelineLane::Model | TimelineLane::Tools
    )));

    let mut default_cache = TimelineSearchCache::build_records(&records, 1, "", false, false);
    assert!(matches!(default_cache.rows, TimelineRows::All(100_000)));
    assert_eq!(default_cache.inspected_records, 0);
    assert_eq!(default_cache.materialized_row_rebuilds, 0);

    let mut updated = records[77_777].as_ref().clone();
    updated.search_text = "needle".into();
    records.set(77_777, Arc::new(updated));
    default_cache.sync_changed_records(&records, 2, [77_777]);
    assert_eq!(default_cache.inspected_records, 0);
    assert!(matches!(default_cache.rows, TimelineRows::All(100_000)));
    assert_eq!(default_cache.materialized_row_rebuilds, 0);

    let mut search_cache = TimelineSearchCache::build_records(&records, 2, "absent", false, false);
    assert_eq!(search_cache.inspected_records, 100_000);
    search_cache.sync_model_matches(geometry, model, model_cache.model_revision, &Vector::new());
    assert!(matches!(
        search_cache.matched_cells,
        TimelineCellMatches::Filtered(ref cells) if cells.is_empty()
    ));
    let inspected_before = search_cache.inspected_records;
    let row_rebuilds_before = search_cache.materialized_row_rebuilds;
    let mut updated = records[77_777].as_ref().clone();
    updated.search_text = "absent now matches".into();
    records.set(77_777, Arc::new(updated));
    let changed = search_cache.sync_changed_records(&records, 3, [77_777]);
    search_cache.sync_model_matches(geometry, model, model_cache.model_revision, &changed);
    assert_eq!(search_cache.inspected_records - inspected_before, 1);
    assert_eq!(search_cache.rows.len(), 1);
    assert_eq!(
        search_cache.rows.get(0),
        Some(TimelineLedgerRow::Record(77_777))
    );
    assert_eq!(search_cache.materialized_row_rebuilds, row_rebuilds_before);
    let matched_cell = geometry
        .render_cell_for_record(&model.cells, 77_777)
        .expect("record is projected");
    assert!(search_cache.matched_cells.contains(matched_cell));

    // A later timing/usage-only receipt advances no search revision and inspects nothing.
    let inspected_before = search_cache.inspected_records;
    search_cache.sync_changed_records(&records, 3, std::iter::empty());
    assert_eq!(search_cache.inspected_records, inspected_before);
}

#[test]
fn zoomed_hundred_thousand_row_timeline_resolves_records_without_a_dense_lookup() {
    let records = (0..100_000_u64)
        .map(|index| Arc::new(record(index + 1, index * 100, index * 100 + 100)))
        .collect::<Vector<_>>();
    let geometry = super::timeline_geometry_from_iter(
        records.iter(),
        AxisId {
            document_generation: 1,
            geometry_revision: 1,
            mode: TimelineMode::Sequence,
        },
    )
    .expect("sequence geometry");
    let model = super::project_timeline(
        &geometry,
        DomainRange::new(50_000.0, 50_100.0),
        1_500.0,
        records.len(),
    );

    assert!(
        geometry
            .render_cell_for_record(&model.cells, 50_050)
            .is_some()
    );
    assert!(geometry.render_cell_for_record(&model.cells, 10).is_none());
}

#[test]
fn empty_query_incremental_append_updates_filtered_rows() {
    let mut records = [Arc::new(record(1, 0, 100))]
        .into_iter()
        .collect::<Vector<_>>();
    let mut collapsed_turns = TimelineSearchCache::build_records(&records, 1, "", true, false);

    let mut next_turn = record(2, 100, 200);
    next_turn.turn = Some(2);
    records.push_back(Arc::new(next_turn));
    collapsed_turns.sync_changed_records(&records, 2, [1]);
    assert_eq!(collapsed_turns.rows.len(), 2);
    assert_eq!(
        collapsed_turns.rows.get(0),
        Some(TimelineLedgerRow::Record(0))
    );
    assert_eq!(
        collapsed_turns.rows.get(1),
        Some(TimelineLedgerRow::Record(1))
    );

    let mut collapsed_calls = TimelineSearchCache::build_records(&records, 2, "", false, true);
    let mut assistant = record(3, 200, 300);
    assistant.id = TrajectoryItemId::Assistant(RequestId::from("request-3"));
    assistant.kind = TrajectoryKind::Assistant;
    records.push_back(Arc::new(assistant));
    collapsed_calls.sync_changed_records(&records, 3, [2]);
    assert_eq!(collapsed_calls.rows.len(), 3);
    assert_eq!(
        collapsed_calls.rows.get(2),
        Some(TimelineLedgerRow::Record(2))
    );
}

#[test]
fn active_search_ignores_turn_and_call_collapsing() {
    let mut tool = record(1, 0, 100);
    tool.search_text = "shell\nneedle".into();
    let records = [Arc::new(tool)].into_iter().collect::<Vector<_>>();

    let cache = TimelineSearchCache::build_records(&records, 1, "needle", true, true);

    assert_eq!(cache.matching_indices.len(), 1);
    assert_eq!(cache.rows.len(), 1);
    assert_eq!(cache.rows.get(0), Some(TimelineLedgerRow::Record(0)));
}

#[test]
fn collapsed_turn_keeps_its_first_record_and_adds_a_summary_row() {
    let mut first = record(1, 0, 100);
    first.id = TrajectoryItemId::Assistant(RequestId::from("request-1"));
    first.kind = TrajectoryKind::Assistant;
    let second = record(2, 100, 200);
    let mut third = record(3, 200, 300);
    third.id = TrajectoryItemId::Assistant(RequestId::from("request-3"));
    third.kind = TrajectoryKind::Assistant;
    third.step = Some(2);
    let records = [Arc::new(first), Arc::new(second), Arc::new(third)]
        .into_iter()
        .collect::<Vector<_>>();

    let cache = TimelineSearchCache::build_records(&records, 1, "", true, false);

    assert_eq!(
        cache.rows,
        TimelineRows::Projected(
            [
                TimelineLedgerRow::Record(0),
                TimelineLedgerRow::TurnSummary {
                    representative: 0,
                    turn: 1,
                    first_hidden: 1,
                    last_hidden: 2,
                    step_ids: [1_u32, 2_u32].into_iter().collect(),
                    call_count: 1,
                },
            ]
            .into_iter()
            .collect()
        )
    );
}

#[test]
fn collapsed_calls_keep_the_assistant_and_add_one_summary_for_its_tool_run() {
    let mut assistant = record(1, 0, 100);
    assistant.id = TrajectoryItemId::Assistant(RequestId::from("request-1"));
    assistant.kind = TrajectoryKind::Assistant;
    let first_tool = record(2, 100, 200);
    let second_tool = record(3, 200, 300);
    let records = [
        Arc::new(assistant),
        Arc::new(first_tool),
        Arc::new(second_tool),
    ]
    .into_iter()
    .collect::<Vector<_>>();

    let cache = TimelineSearchCache::build_records(&records, 1, "", false, true);

    assert_eq!(
        cache.rows,
        TimelineRows::Projected(
            [
                TimelineLedgerRow::Record(0),
                TimelineLedgerRow::CallsSummary {
                    assistant: 0,
                    first_tool: 1,
                    last_tool: 2,
                    tool_names: ["tool".to_owned()].into_iter().collect(),
                    tools: Arc::from("tool"),
                    tools_truncated: false,
                },
            ]
            .into_iter()
            .collect()
        )
    );
}

#[test]
fn collapsed_calls_preserve_standalone_tools_and_deduplicate_names_in_source_order() {
    let mut assistant = record(1, 0, 100);
    assistant.id = TrajectoryItemId::Assistant(RequestId::from("request-1"));
    assistant.kind = TrajectoryKind::Assistant;
    let mut bash_first = record(2, 100, 200);
    bash_first.title = "bash".into();
    let mut bash_second = record(3, 200, 300);
    bash_second.title = "bash".into();
    let mut read = record(4, 300, 400);
    read.title = "read".into();
    let mut context = record(5, 400, 500);
    context.kind = TrajectoryKind::Context;
    let mut standalone = record(6, 500, 600);
    standalone.title = "standalone".into();
    let records = [
        Arc::new(assistant),
        Arc::new(bash_first),
        Arc::new(bash_second),
        Arc::new(read),
        Arc::new(context),
        Arc::new(standalone),
    ]
    .into_iter()
    .collect::<Vector<_>>();

    let cache = TimelineSearchCache::build_records(&records, 1, "", false, true);

    assert_eq!(cache.rows.len(), 4);
    assert_eq!(cache.rows.get(0), Some(TimelineLedgerRow::Record(0)));
    assert_eq!(
        cache.rows.get(1),
        Some(TimelineLedgerRow::CallsSummary {
            assistant: 0,
            first_tool: 1,
            last_tool: 3,
            tool_names: ["bash".to_owned(), "read".to_owned()].into_iter().collect(),
            tools: Arc::from("bash, read"),
            tools_truncated: false,
        })
    );
    assert_eq!(cache.rows.get(2), Some(TimelineLedgerRow::Record(4)));
    assert_eq!(cache.rows.get(3), Some(TimelineLedgerRow::Record(5)));
    assert!(cache.rows.get(1).unwrap().represents(2, &records));
    assert!(!cache.rows.get(1).unwrap().represents(5, &records));
}

#[test]
fn folded_projection_preserves_system_rows_and_incremental_equals_full_replay() {
    let mut system_before = record(1, 0, 100);
    system_before.kind = TrajectoryKind::System;
    let mut assistant = record(2, 100, 200);
    assistant.id = TrajectoryItemId::Assistant(RequestId::from("request-2"));
    assistant.kind = TrajectoryKind::Assistant;
    let mut tool = record(3, 200, 300);
    tool.title = "bash".into();
    let mut system_middle = record(4, 300, 400);
    system_middle.kind = TrajectoryKind::System;
    let mut second_tool = record(5, 400, 500);
    second_tool.title = "read".into();
    second_tool.step = Some(2);
    let source = [system_before, assistant, tool, system_middle, second_tool];
    let collapsed_turns = HashSet::from([1]);
    let collapsed_assistants = HashSet::from([source[1].id.clone()]);

    let mut records = Vector::new();
    let mut cache = TimelineSearchCache::build_records_with_folds(
        &records,
        1,
        "",
        0,
        0,
        &collapsed_turns,
        &collapsed_assistants,
    );
    for (index, record) in source.into_iter().enumerate() {
        records.push_back(Arc::new(record));
        cache.sync_changed_records(&records, index as u64 + 2, [index]);
        assert_eq!(
            cache.rows,
            super::project_ledger_rows(
                &records,
                &super::TimelineMatches::All(records.len()),
                false,
                &collapsed_turns,
                &collapsed_assistants,
            ),
            "incremental projection diverged at prefix length {}",
            index + 1,
        );
    }
    assert_eq!(cache.rows.get(0), Some(TimelineLedgerRow::Record(0)));
    assert_eq!(cache.rows.get(1), Some(TimelineLedgerRow::Record(1)));
    assert!(matches!(
        cache.rows.get(2),
        Some(TimelineLedgerRow::TurnSummary {
            first_hidden: 2,
            last_hidden: 4,
            ..
        })
    ));
    assert_eq!(cache.rows.get(3), Some(TimelineLedgerRow::Record(3)));
}

#[test]
fn folded_summary_focus_covers_only_its_hidden_members() {
    let mut assistant = record(1, 0, 100);
    assistant.id = TrajectoryItemId::Assistant(RequestId::from("request-1"));
    assistant.kind = TrajectoryKind::Assistant;
    let records = [Arc::new(assistant), Arc::new(record(2, 100, 200))]
        .into_iter()
        .collect::<Vector<_>>();
    let cache = TimelineSearchCache::build_records(&records, 1, "", true, false);
    let summary = cache.rows.get(1).expect("turn summary");

    assert!(!summary.intersects(&HashSet::from([0]), &records));
    assert!(summary.intersects(&HashSet::from([1]), &records));
}

#[test]
fn ten_thousand_folded_appends_do_not_rebuild_the_existing_projection() {
    let mut records = Vector::new();
    let collapsed_turns = (1..=2_500_u32).collect::<HashSet<_>>();
    let collapsed_assistants = HashSet::new();
    let mut cache = TimelineSearchCache::build_records_with_folds(
        &records,
        1,
        "",
        0,
        0,
        &collapsed_turns,
        &collapsed_assistants,
    );
    for index in 0..10_000_usize {
        let mut next = record(index as u64 + 1, index as u64, index as u64 + 100);
        next.turn = Some((index / 4 + 1) as u32);
        next.step = Some((index / 2 + 1) as u32);
        if index % 4 == 0 {
            next.id = TrajectoryItemId::Assistant(RequestId::from(format!("request-{index}")));
            next.kind = TrajectoryKind::Assistant;
        }
        records.push_back(Arc::new(next));
        cache.sync_changed_records(&records, index as u64 + 2, [index]);
    }

    assert_eq!(cache.inspected_records, 10_000);
    assert_eq!(cache.materialized_row_rebuilds, 1);
    assert_eq!(
        cache.rows,
        super::project_ledger_rows(
            &records,
            &super::TimelineMatches::All(records.len()),
            false,
            &collapsed_turns,
            &collapsed_assistants,
        )
    );
}

#[test]
fn alternating_system_rows_keep_collapsed_turn_appends_linear_and_canonical() {
    let mut records = Vector::new();
    let collapsed_turns = HashSet::from([1]);
    let collapsed_assistants = HashSet::new();
    let mut cache = TimelineSearchCache::build_records_with_folds(
        &records,
        1,
        "",
        0,
        0,
        &collapsed_turns,
        &collapsed_assistants,
    );
    for index in 0..10_000_usize {
        let start = index as u64 * 100;
        let mut next = record(index as u64 + 1, start, start + 100);
        next.turn = Some(1);
        next.step = Some((index / 2 + 1) as u32);
        if index % 2 == 1 {
            next.kind = TrajectoryKind::System;
        }
        records.push_back(Arc::new(next));
        cache.sync_changed_records(&records, index as u64 + 2, [index]);
    }

    assert_eq!(cache.materialized_row_rebuilds, 1);
    assert_eq!(
        cache.rows,
        super::project_ledger_rows(
            &records,
            &super::TimelineMatches::All(records.len()),
            false,
            &collapsed_turns,
            &collapsed_assistants,
        )
    );
}

#[test]
fn tool_summary_preview_has_bounded_append_cost() {
    let mut preview: Arc<str> = Arc::from("");
    let mut truncated = false;
    for index in 0..10_000 {
        (preview, truncated) =
            super::append_tool_summary_preview(&preview, truncated, &format!("tool-{index}"));
    }

    assert!(truncated);
    assert!(preview.len() <= super::TOOL_SUMMARY_PREVIEW_MAX_BYTES + ", …".len());
    assert!(preview.ends_with('…'));
}

#[test]
fn filtered_rows_recompute_turn_start_for_double_click_and_connectors() {
    let records = [Arc::new(record(1, 0, 100)), Arc::new(record(2, 100, 200))]
        .into_iter()
        .collect::<Vector<_>>();
    let matching = super::TimelineMatches::Filtered([1_usize].into_iter().collect());
    let rows =
        super::project_ledger_rows(&records, &matching, true, &HashSet::new(), &HashSet::new());

    assert_eq!(
        super::ledger_record_boundaries(&rows, 0, &records[1], &records),
        (true, false, false)
    );
    assert_eq!(
        super::ledger_double_click_target(
            &records[1],
            true,
            &HashSet::new(),
            &HashSet::from([1]),
            &HashSet::new(),
        ),
        Some(super::LedgerFoldTarget::Turn(1))
    );
}

#[test]
fn multi_term_search_updates_only_the_changed_record() {
    let mut records = (0..100_000)
        .map(|index| Arc::new(record(index, index, index.saturating_add(100))))
        .collect::<Vector<_>>();
    let target = 77_777;
    let mut initial = records[target].as_ref().clone();
    initial.search_text = "alpha".into();
    records.set(target, Arc::new(initial));
    let mut cache = TimelineSearchCache::build_records(&records, 1, "alpha beta", true, true);
    assert_eq!(cache.rows.len(), 0);

    let inspected = cache.inspected_records;
    let rebuilds = cache.materialized_row_rebuilds;
    let mut matching = records[target].as_ref().clone();
    matching.search_text = "alpha unrelated beta".into();
    records.set(target, Arc::new(matching));
    cache.sync_changed_records(&records, 2, [target]);
    assert_eq!(cache.inspected_records - inspected, 1);
    assert_eq!(cache.materialized_row_rebuilds, rebuilds);
    assert_eq!(cache.rows.get(0), Some(TimelineLedgerRow::Record(target)));

    let inspected = cache.inspected_records;
    let mut no_longer_matching = records[target].as_ref().clone();
    no_longer_matching.search_text = "beta only".into();
    records.set(target, Arc::new(no_longer_matching));
    cache.sync_changed_records(&records, 3, [target]);
    assert_eq!(cache.inspected_records - inspected, 1);
    assert_eq!(cache.rows.len(), 0);
}

#[test]
fn text_only_changes_advance_geometry_cursor_without_rebuilding_focus() {
    let events = fixture();
    let mut document = SessionDocument::from_events(events[..7].to_vec()).unwrap();
    let mut projection = TrajectoryProjection::from_document(&document);
    let request_id = RequestId::from("request-1");
    let first = recorded(
        document.cursor().next_seq,
        SessionEvent::AssistantChunk {
            request_id: request_id.clone(),
            chunk: AssistantChunk::OutputTextDelta { delta: "a".into() },
        },
    );
    let delta = document.apply_batch(vec![first]).unwrap();
    projection = TrajectoryProjection::after_delta(&document, &delta, &projection);

    let axis = AxisId {
        document_generation: projection.projection_lineage(),
        geometry_revision: projection.revision(),
        mode: TimelineMode::Sequence,
    };
    let assistant_index = projection
        .record_index(&TrajectoryItemId::Assistant(request_id.clone()))
        .unwrap();
    let selection = AxisRange {
        axis,
        range: DomainRange::new(assistant_index as f64, assistant_index as f64 + 1.0),
    };
    let mut cache = TimelineModelCache::new(
        cache_identity(
            projection.projection_lineage(),
            projection.revision(),
            TimelineMode::Sequence,
        ),
        &projection.records,
        TimelineView {
            viewport: None,
            selection: Some(selection),
            render_width_px: 1_500.0,
        },
        None,
    );
    let mut hidden_cache = TimelineModelCache::new(
        cache_identity(
            projection.projection_lineage(),
            projection.revision(),
            TimelineMode::Sequence,
        ),
        &projection.records,
        TimelineView {
            viewport: None,
            selection: Some(selection),
            render_width_px: 1_500.0,
        },
        None,
    );
    // Geometry and field-change cursors are independent (input attachment changes layout).
    cache.change_revision = projection.change_revision();
    hidden_cache.change_revision = projection.change_revision();
    let focused = Arc::clone(&cache.focus.as_ref().unwrap().record_indices);

    for _ in 0..300 {
        let event = recorded(
            document.cursor().next_seq,
            SessionEvent::AssistantChunk {
                request_id: request_id.clone(),
                chunk: AssistantChunk::OutputTextDelta { delta: "x".into() },
            },
        );
        let delta = document.apply_batch(vec![event]).unwrap();
        projection = TrajectoryProjection::after_delta(&document, &delta, &projection);
        let focus_changed = cache.sync_projection(&projection).unwrap();
        assert!(!focus_changed);
        cache.sync_focus(&projection.records, Some(selection), focus_changed);
    }
    assert_eq!(cache.change_revision, projection.change_revision());
    assert!(Arc::ptr_eq(
        &focused,
        &cache.focus.as_ref().unwrap().record_indices
    ));

    let completion = events
        .iter()
        .find_map(|event| {
            matches!(event.event, SessionEvent::AssistantCompleted { .. })
                .then(|| event.event.clone())
        })
        .unwrap();
    let delta = document
        .apply_batch(vec![recorded(document.cursor().next_seq, completion)])
        .unwrap();
    projection = TrajectoryProjection::after_delta(&document, &delta, &projection);
    assert_eq!(cache.sync_projection(&projection), Some(true));
    assert_eq!(hidden_cache.sync_projection(&projection), Some(true));
    assert_eq!(cache.change_revision, projection.change_revision());
    assert!(cache.geometry_matches(AxisId {
        geometry_revision: projection.revision(),
        ..axis
    }));
}

#[test]
fn timed_cache_tail_update_matches_a_fresh_projection_without_rebuilding_geometry() {
    let events = fixture();
    let mut document = SessionDocument::from_events(events[..27].to_vec()).unwrap();
    let mut projection = TrajectoryProjection::from_document(&document);
    let mut caches = [TimelineMode::Duration, TimelineMode::Actual].map(|mode| {
        TimelineModelCache::new(
            TimelineCacheIdentity {
                axis: AxisId {
                    document_generation: projection.projection_lineage(),
                    geometry_revision: projection.revision(),
                    mode,
                },
                change_revision: projection.change_revision(),
            },
            &projection.records,
            TimelineView {
                viewport: None,
                selection: None,
                render_width_px: 1_500.0,
            },
            None,
        )
    });

    let delta = document.apply_batch(vec![events[27].clone()]).unwrap();
    projection = TrajectoryProjection::after_delta(&document, &delta, &projection);

    for (cache, mode) in caches
        .iter_mut()
        .zip([TimelineMode::Duration, TimelineMode::Actual])
    {
        assert_eq!(cache.sync_projection(&projection), Some(true));
        assert_eq!(cache.timed_incremental_updates, 1);
        let rebuilt = TimelineModelCache::new(
            TimelineCacheIdentity {
                axis: AxisId {
                    document_generation: projection.projection_lineage(),
                    geometry_revision: projection.revision(),
                    mode,
                },
                change_revision: projection.change_revision(),
            },
            &projection.records,
            TimelineView {
                viewport: None,
                selection: None,
                render_width_px: 1_500.0,
            },
            None,
        );
        let incremental = cache.model.as_ref().unwrap();
        let fresh = rebuilt.model.as_ref().unwrap();
        assert_eq!(incremental.axis, fresh.axis);
        assert_eq!(incremental.domain, fresh.domain);
        assert_eq!(incremental.viewport, fresh.viewport);
        assert_eq!(incremental.cells, fresh.cells);
    }
}

#[test]
fn timed_ranges_survive_streaming_assistant_and_tool_completion() {
    let events = fixture();
    for (prefix_len, completion_index) in [(9_usize, 9_usize), (15, 15)] {
        let mut document = SessionDocument::from_events(events[..prefix_len].to_vec()).unwrap();
        let mut projection = TrajectoryProjection::from_document(&document);

        let mut cases = [TimelineMode::Duration, TimelineMode::Actual].map(|mode| {
            let axis = AxisId {
                document_generation: projection.projection_lineage(),
                geometry_revision: projection.revision(),
                mode,
            };
            let initial = TimelineModelCache::new(
                TimelineCacheIdentity {
                    axis,
                    change_revision: projection.change_revision(),
                },
                &projection.records,
                TimelineView {
                    viewport: None,
                    selection: None,
                    render_width_px: 1_500.0,
                },
                None,
            );
            let domain = initial.geometry.as_ref().unwrap().domain;
            let viewport = AxisRange {
                axis,
                range: DomainRange::new(
                    domain.start + domain.width() * 0.1,
                    domain.start + domain.width() * 0.9,
                ),
            };
            let selection = AxisRange {
                axis,
                range: DomainRange::new(
                    domain.start + domain.width() * 0.25,
                    domain.start + domain.width() * 0.55,
                ),
            };
            let cache = TimelineModelCache::new(
                TimelineCacheIdentity {
                    axis,
                    change_revision: projection.change_revision(),
                },
                &projection.records,
                TimelineView {
                    viewport: Some(viewport),
                    selection: Some(selection),
                    render_width_px: 1_500.0,
                },
                None,
            );
            (cache, viewport, selection)
        });

        let delta = document
            .apply_batch(vec![events[completion_index].clone()])
            .unwrap();
        projection = TrajectoryProjection::after_delta(&document, &delta, &projection);
        for (cache, viewport, selection) in &mut cases {
            assert_ne!(projection.revision(), selection.axis.geometry_revision);
            if let Some(focus_changed) = cache.sync_projection(&projection) {
                cache.sync_focus(&projection.records, Some(*selection), focus_changed);
            } else {
                *cache = TimelineModelCache::new(
                    TimelineCacheIdentity {
                        axis: AxisId {
                            document_generation: projection.projection_lineage(),
                            geometry_revision: projection.revision(),
                            mode: selection.axis.mode,
                        },
                        change_revision: projection.change_revision(),
                    },
                    &projection.records,
                    TimelineView {
                        viewport: Some(*viewport),
                        selection: Some(*selection),
                        render_width_px: 1_500.0,
                    },
                    None,
                );
            }

            let geometry = cache.geometry.as_ref().unwrap();
            let rebound_selection = cache
                .display_selection(Some(*selection))
                .expect("same-session timed selection must survive completion timing");
            assert_eq!(rebound_selection.axis, geometry.axis);
            assert_eq!(
                rebound_selection.range,
                selection.range.clamp_to(geometry.domain)
            );
            assert!(cache.focus.is_some());
            let rebound_viewport = viewport.range.clamp_to(geometry.domain);
            assert_eq!(cache.resolved_viewport(Some(*viewport)), rebound_viewport);
            assert_eq!(cache.model.as_ref().unwrap().viewport, rebound_viewport);
        }
    }
}

#[test]
fn selection_focus_is_materialized_once_for_overview_and_ledger() {
    let projection_lineage = 91;
    let revision = 4;
    let records = (0..100)
        .map(|index| Arc::new(record(index, index, index.saturating_add(100))))
        .collect::<im::Vector<_>>();
    let axis = AxisId {
        document_generation: projection_lineage,
        geometry_revision: revision,
        mode: TimelineMode::Sequence,
    };
    let selection = AxisRange {
        axis,
        range: DomainRange::new(10.0, 20.0),
    };
    let mut cache = TimelineModelCache::new(
        cache_identity(projection_lineage, revision, TimelineMode::Sequence),
        &records,
        TimelineView {
            viewport: None,
            selection: Some(selection),
            render_width_px: 1_500.0,
        },
        None,
    );

    cache.sync_focus(&records, Some(selection), false);
    let first = cache.focus.as_ref().unwrap();
    let record_indices = Arc::clone(&first.record_indices);
    cache.sync_focus(&records, Some(selection), false);
    let second = cache.focus.as_ref().unwrap();

    assert!(Arc::ptr_eq(&record_indices, &second.record_indices));
}

#[test]
fn focus_interval_prefixes_exclude_system_records_only_for_turn_summaries() {
    let mut system = record(2, 100, 200);
    system.kind = TrajectoryKind::System;
    let mut trailing_system = record(5, 400, 500);
    trailing_system.kind = TrajectoryKind::System;
    let records = [
        record(1, 0, 100),
        system,
        record(3, 200, 300),
        record(4, 300, 400),
        trailing_system,
    ]
    .into_iter()
    .map(Arc::new)
    .collect::<Vector<_>>();
    let axis = AxisId {
        document_generation: 93,
        geometry_revision: 1,
        mode: TimelineMode::Sequence,
    };
    let focus = TimelineFocusCache::new(
        AxisRange {
            axis,
            range: DomainRange::new(1.0, 4.0),
        },
        &records,
        HashSet::from([1, 3]),
    );

    assert!(focus.intersects(0, 2));
    assert!(!focus.intersects_non_system(0, 2));
    assert!(focus.intersects(2, 4));
    assert!(focus.intersects_non_system(2, 4));
    assert!(!focus.intersects(0, 0));
    assert!(!focus.intersects(records.len(), usize::MAX));
}

#[test]
fn changed_matches_rescan_each_affected_cluster_once() {
    let count = 256;
    let records = (0..count)
        .map(|index| Arc::new(record(index as u64, index as u64, index as u64 + 100)))
        .collect::<Vector<_>>();
    let axis = AxisId {
        document_generation: 94,
        geometry_revision: 1,
        mode: TimelineMode::Sequence,
    };
    let domain = DomainRange::new(0.0, count as f64);
    let model = TimelineModel {
        axis,
        domain,
        viewport: domain,
        render_width_px: 1_500.0,
        cells: vec![RenderCell {
            ids: RenderIdentity::explicit((0..count).collect()),
            lane: TimelineLane::Tools,
            start_px: 0.0,
            end_px: 1_500.0,
            nested: None,
            clustered: true,
        }],
    };
    let geometry = super::timeline_geometry_from_iter(records.iter(), axis).unwrap();
    let mut cache = TimelineSearchCache::build_records(&records, 1, "absent", false, false);
    cache.sync_model_matches(&geometry, &model, 7, &Vector::new());
    cache.matching_indices = TimelineMatches::Filtered([count - 1].into_iter().collect());
    let changed = (0..count).collect::<Vector<_>>();

    cache.sync_model_matches(&geometry, &model, 7, &changed);
    assert_eq!(cache.matched_cell_rescans, 1);
    assert!(matches!(
        cache.matched_cells,
        TimelineCellMatches::Filtered(ref cells) if cells.contains(&0)
    ));

    cache.matching_indices = TimelineMatches::Filtered(ImHashSet::new());
    cache.sync_model_matches(&geometry, &model, 7, &changed);
    assert_eq!(cache.matched_cell_rescans, 2);
    assert!(matches!(
        cache.matched_cells,
        TimelineCellMatches::Filtered(ref cells) if cells.is_empty()
    ));
}

#[test]
fn retained_search_invalidates_its_cell_projection_in_a_new_model_cache() {
    let records = [record(1, 0, 100), record(2, 100, 200)]
        .into_iter()
        .map(Arc::new)
        .collect::<im::Vector<_>>();
    let mut first = TimelineModelCache::new(
        cache_identity(92, 1, TimelineMode::Sequence),
        &records,
        TimelineView {
            viewport: None,
            selection: None,
            render_width_px: 1_500.0,
        },
        Some(TimelineSearchCache::build_records(
            &records, 1, "tool", false, false,
        )),
    );
    let model = first.model.as_ref().unwrap();
    let geometry = first.geometry.as_ref().unwrap();
    first.search.as_mut().unwrap().sync_model_matches(
        geometry,
        model,
        first.model_revision,
        &Vector::new(),
    );
    assert_eq!(
        first.search.as_ref().unwrap().matched_model_revision,
        first.model_revision
    );

    let second = TimelineModelCache::new(
        cache_identity(92, 1, TimelineMode::Duration),
        &records,
        TimelineView {
            viewport: None,
            selection: None,
            render_width_px: 1_500.0,
        },
        first.search.take(),
    );
    assert_eq!(
        second.search.as_ref().unwrap().matched_model_revision,
        u64::MAX
    );
}

#[test]
fn timeline_cache_does_not_rebind_ranges_across_lineage_or_mode() {
    let projection_lineage = 18;
    let records = [record(1, 0, 100)]
        .into_iter()
        .map(Arc::new)
        .collect::<im::Vector<_>>();
    let foreign_lineage_axis = AxisId {
        document_generation: projection_lineage - 1,
        geometry_revision: 3,
        mode: TimelineMode::Duration,
    };
    let cache = TimelineModelCache::new(
        cache_identity(projection_lineage, 4, TimelineMode::Duration),
        &records,
        TimelineView {
            viewport: Some(AxisRange {
                axis: foreign_lineage_axis,
                range: DomainRange::new(20.0, 40.0),
            }),
            selection: Some(AxisRange {
                axis: foreign_lineage_axis,
                range: DomainRange::new(25.0, 30.0),
            }),
            render_width_px: 1_500.0,
        },
        None,
    );

    let model = cache.model.as_ref().unwrap();
    assert_eq!(model.viewport, model.domain);
    assert_eq!(
        cache.display_selection(Some(AxisRange {
            axis: foreign_lineage_axis,
            range: DomainRange::new(25.0, 30.0),
        })),
        None
    );
    assert_eq!(
        cache.display_selection(Some(AxisRange {
            axis: AxisId {
                document_generation: projection_lineage,
                geometry_revision: 3,
                mode: TimelineMode::Actual,
            },
            range: DomainRange::new(25.0, 30.0),
        })),
        None
    );
}

#[test]
fn viewport_and_selection_are_clamped_and_normalized() {
    let domain = DomainRange::new(0.0, 20.0);
    assert_eq!(
        DomainRange::new(-5.0, 5.0).clamp_to(domain),
        DomainRange::new(0.0, 10.0)
    );
    assert_eq!(
        normalized_range(DomainRange::new(5.0, 10.0), domain),
        (0.25, 0.25)
    );
}

#[test]
fn timeline_modes_keep_distinct_coordinate_semantics() {
    let records = [record(1, 0, 100), record(2, 200, 250)];
    let sequence = timeline_model(&records, TimelineMode::Sequence, None).unwrap();
    assert_eq!(sequence.domain, DomainRange::new(0.0, 2.0));
    let actual = timeline_model(&records, TimelineMode::Actual, None).unwrap();
    assert_eq!(actual.domain, DomainRange::new(1_000.0, 1_250.0));
    let duration = timeline_model(&records, TimelineMode::Duration, None).unwrap();
    assert_eq!(duration.domain, DomainRange::new(0.0, 150.0));
}

#[test]
fn actual_timeline_nests_execution_inside_tool_lifecycle() {
    let model = timeline_model(&[record(1, 0, 100)], TimelineMode::Actual, None).unwrap();
    let cell = &model.cells[0];
    assert!((cell.start_px - 0.0).abs() < 0.000_001);
    assert!((cell.end_px - model.render_width_px).abs() < 0.000_001);
    let (left, width) = nested_segment_geometry(cell).unwrap();
    assert!((left - 0.2).abs() < 0.000_001);
    assert!((width - 0.6).abs() < 0.000_001);
}

#[test]
fn timeline_hover_hits_only_bars_and_prefers_the_topmost_record() {
    let records = [record(1, 0, 100), record(2, 0, 100)];
    let geometry = timeline_geometry(
        &records,
        AxisId {
            document_generation: 1,
            geometry_revision: 1,
            mode: TimelineMode::Actual,
        },
    )
    .unwrap();
    let model = timeline_model(&records, TimelineMode::Actual, None).unwrap();

    for lane in [
        TimelineLane::Input,
        TimelineLane::Model,
        TimelineLane::Tools,
    ] {
        let top = timeline_lane_top(lane) + TIMELINE_BAR_OFFSET;
        assert_eq!(timeline_lane_at(top), Some(lane));
        assert_eq!(timeline_lane_at(top + TIMELINE_BAR_HEIGHT), Some(lane));
    }
    assert_eq!(timeline_lane_at(7.0), Some(TimelineLane::Input));
    assert_eq!(timeline_lane_at(20.0), None);
    assert_eq!(timeline_lane_at(35.0), Some(TimelineLane::Tools));
    assert_eq!(geometry.hit_test(TimelineLane::Tools, 1_050.0), Some(&1));
    assert_eq!(geometry.hit_test(TimelineLane::Model, 1_050.0), None);
    assert_eq!(model.hit_test(TimelineLane::Tools, 0.5), Some(1));
}

#[test]
fn assistant_hover_tooltip_uses_dsh_timing_shape() {
    let mut assistant = record(1, 0, 100);
    assistant.kind = TrajectoryKind::Assistant;
    assistant.timing.first_token = Some((&time(20)).into());

    let tooltip = record_tooltip(&assistant);
    assert!(tooltip.starts_with("ASSISTANT\n"));
    assert!(tooltip.contains(" → "));
    assert!(tooltip.contains("Total 100 ms"));
    assert!(tooltip.contains("TTFT 20 ms · Decoding 80 ms"));
}

#[test]
fn timing_labels_follow_dsh_rounding_and_missing_value_rules() {
    assert_eq!(
        super::format_assistant_duration(Some(999_400_000)),
        "999 ms"
    );
    assert_eq!(
        super::format_assistant_duration(Some(1_234_000_000)),
        "1.23 s"
    );
    assert_eq!(
        super::format_assistant_duration(Some(12_340_000_000)),
        "12.3 s"
    );
    assert_eq!(
        super::format_elapsed_duration(Some(1_234_600_000)),
        "1,235 ms"
    );
    assert_eq!(super::format_elapsed_duration(None), "—");
}

#[test]
fn details_tabs_follow_dsh_role_descriptors_without_inventing_data() {
    let shape = |record: &TrajectoryRecord| {
        super::relevant_record_tabs(record, None)
            .into_iter()
            .map(|descriptor| (descriptor.tab, descriptor.label))
            .collect::<Vec<_>>()
    };

    let mut system = record(1, 0, 100);
    system.kind = TrajectoryKind::System;
    assert_eq!(
        shape(&system),
        vec![
            (DetailsTab::SystemPrompt, "System Prompt"),
            (DetailsTab::Tools, "Tools"),
        ]
    );

    let mut assistant = record(2, 0, 100);
    assistant.kind = TrajectoryKind::Assistant;
    assert_eq!(
        shape(&assistant),
        vec![
            (DetailsTab::Summary, "Summary"),
            (DetailsTab::Preview, "Preview"),
            (DetailsTab::Raw, "Raw"),
        ]
    );

    let mut tool = record(3, 0, 100);
    tool.text.clear();
    tool.payload = None;
    assert_eq!(
        shape(&tool),
        vec![
            (DetailsTab::Summary, "Summary"),
            (DetailsTab::Schema, "Schema"),
            (DetailsTab::Timing, "Timing"),
        ]
    );
    tool.payload = Some(r#"{"path":"README.md"}"#.into());
    tool.text = "done".into();
    assert_eq!(
        shape(&tool),
        vec![
            (DetailsTab::Summary, "Summary"),
            (DetailsTab::Payload, "Payload"),
            (DetailsTab::Result, "Result"),
            (DetailsTab::Schema, "Schema"),
            (DetailsTab::Timing, "Timing"),
        ]
    );

    let mut compaction = record(4, 0, 100);
    compaction.kind = TrajectoryKind::Compaction;
    assert_eq!(
        shape(&compaction),
        vec![
            (DetailsTab::Summary, "Summary"),
            (DetailsTab::Raw, "Raw Output"),
        ]
    );
}

#[test]
fn canonical_prompt_schema_and_request_options_drive_inspector_tabs() {
    let document = SessionDocument::from_events(fixture()).unwrap();
    let projection = TrajectoryProjection::from_document(&document);

    let system = projection
        .records
        .iter()
        .find(|record| record.kind == TrajectoryKind::System)
        .unwrap();
    let system_details = projection.record_details(&system.id);
    let system_tabs = super::relevant_record_tabs(system, system_details)
        .into_iter()
        .map(|descriptor| descriptor.tab)
        .collect::<Vec<_>>();
    assert!(system_tabs.contains(&DetailsTab::SystemPrompt));
    assert!(system_tabs.contains(&DetailsTab::Tools));

    let tool = projection
        .records
        .iter()
        .find(|record| record.kind == TrajectoryKind::Tool)
        .unwrap();
    let tool_details = projection.record_details(&tool.id);
    let tool_tabs = super::relevant_record_tabs(tool, tool_details)
        .into_iter()
        .map(|descriptor| descriptor.tab)
        .collect::<Vec<_>>();
    assert!(tool_tabs.contains(&DetailsTab::Schema));

    let request = projection.requests.front().unwrap();
    let request_tabs = super::relevant_request_tabs(request)
        .into_iter()
        .map(|descriptor| descriptor.tab)
        .collect::<Vec<_>>();
    assert_eq!(
        request_tabs,
        vec![
            DetailsTab::Summary,
            DetailsTab::Options,
            DetailsTab::Usage,
            DetailsTab::Timing,
        ]
    );
}

#[test]
fn assistant_timing_missing_states_use_dsh_precedence() {
    let mut assistant = record(1, 0, 100);
    assistant.kind = TrajectoryKind::Assistant;
    assistant.timing = RecordTiming::default();

    assert_eq!(super::timing_duration(&assistant), "Not recorded");
    assert_eq!(super::assistant_ttft(&assistant), "Not recorded");
    assert_eq!(
        super::assistant_generation(&assistant),
        "First token unavailable"
    );
    assert_eq!(super::assistant_throughput(&assistant), "Usage unavailable");

    assistant.usage = Some(TokenUsage::default());
    assistant.timing.completed = Some((&time(100)).into());
    assert_eq!(super::timing_duration(&assistant), "Step start unavailable");
    assert_eq!(super::assistant_ttft(&assistant), "Step start unavailable");
    assert_eq!(
        super::assistant_throughput(&assistant),
        "First token unavailable"
    );

    assistant.timing = RecordTiming {
        started: Some((&time(0)).into()),
        first_token: Some((&time(20)).into()),
        ..RecordTiming::default()
    };
    assert_eq!(super::timing_duration(&assistant), "Pending");
    assert_eq!(super::assistant_ttft(&assistant), "20 ms");
    assert_eq!(super::assistant_generation(&assistant), "Pending");
    assert_eq!(super::assistant_throughput(&assistant), "Pending");

    assistant.timing.completed = Some((&time(100)).into());
    assert_eq!(super::assistant_generation(&assistant), "80 ms");
    assert_eq!(super::assistant_throughput(&assistant), "0.0 tok/s");
}

#[test]
fn details_default_width_matches_dsh_clamp() {
    assert_eq!(super::trajectory_details_default_width(761.0), 320.0);
    assert_eq!(super::trajectory_details_default_width(900.0), 342.0);
    assert_eq!(super::trajectory_details_default_width(1_500.0), 440.0);
}

#[test]
fn fold_projection_and_double_click_target_only_the_requested_group() {
    let mut first_assistant = record(1, 0, 100);
    first_assistant.id = TrajectoryItemId::Assistant(RequestId::from("request-1"));
    first_assistant.kind = TrajectoryKind::Assistant;
    let mut first_tool = record(2, 100, 200);
    first_tool.turn = Some(1);
    let mut second_assistant = record(3, 200, 300);
    second_assistant.id = TrajectoryItemId::Assistant(RequestId::from("request-2"));
    second_assistant.kind = TrajectoryKind::Assistant;
    second_assistant.turn = Some(2);
    let mut second_tool = record(4, 300, 400);
    second_tool.turn = Some(2);
    let records = [first_assistant, first_tool, second_assistant, second_tool]
        .into_iter()
        .map(Arc::new)
        .collect::<Vector<_>>();

    let collapsed_turns = HashSet::from([1]);
    let collapsed_assistants = HashSet::from([records[2].id.clone()]);
    let rows = super::project_ledger_rows(
        &records,
        &super::TimelineMatches::All(records.len()),
        false,
        &collapsed_turns,
        &collapsed_assistants,
    );
    assert!(matches!(
        rows.get(1),
        Some(TimelineLedgerRow::TurnSummary { turn: 1, .. })
    ));
    assert_eq!(rows.get(2), Some(TimelineLedgerRow::Record(2)));
    assert!(matches!(
        rows.get(3),
        Some(TimelineLedgerRow::CallsSummary { assistant: 2, .. })
    ));
    let collapsible_turns = HashSet::from([1, 2]);
    let collapsible_assistants = HashSet::from([records[0].id.clone(), records[2].id.clone()]);

    assert_eq!(
        super::ledger_double_click_target(
            &records[0],
            true,
            &collapsed_turns,
            &collapsible_turns,
            &collapsible_assistants,
        ),
        Some(super::LedgerFoldTarget::Turn(1))
    );
    assert_eq!(
        super::ledger_double_click_target(
            &records[2],
            true,
            &collapsed_turns,
            &collapsible_turns,
            &collapsible_assistants,
        ),
        Some(super::LedgerFoldTarget::Assistant(records[2].id.clone()))
    );
}

#[test]
fn clipped_nested_segment_does_not_create_an_invalid_width_range() {
    let cell = RenderCell {
        ids: RenderIdentity::explicit(vec![0]),
        lane: TimelineLane::Tools,
        start_px: 0.0,
        end_px: 100.0,
        nested: Some((100.0, 120.0)),
        clustered: false,
    };
    assert_eq!(nested_segment_geometry(&cell), None);
}

#[test]
fn collapsed_ledger_rows_use_dsh_height_and_summary_copy() {
    let turn = TimelineLedgerRow::TurnSummary {
        representative: 0,
        turn: 1,
        first_hidden: 1,
        last_hidden: 2,
        step_ids: ImHashSet::new(),
        call_count: 2,
    };
    let calls = TimelineLedgerRow::CallsSummary {
        assistant: 0,
        first_tool: 1,
        last_tool: 2,
        tool_names: ImHashSet::new(),
        tools: Arc::from("bash · read"),
        tools_truncated: false,
    };

    assert_eq!(
        trajectory_ledger_row_height(&TimelineLedgerRow::Record(0)),
        30.0
    );
    assert_eq!(
        trajectory_ledger_row_height(&TimelineLedgerRow::RequestBoundary {
            request: 4,
            run_index: 0,
            terminal: true,
        }),
        9.0
    );
    assert_eq!(
        trajectory_ledger_row_height(&TimelineLedgerRow::RequestBoundary {
            request: 3,
            run_index: 0,
            terminal: false,
        }),
        0.0
    );
    assert_eq!(trajectory_ledger_row_height(&turn), 20.0);
    assert_eq!(trajectory_ledger_row_height(&calls), 20.0);
    assert_eq!(turn_summary_text(27, 8), "… 27 steps · 8 tool calls");
    assert_eq!(turn_summary_text(1, 1), "… 1 step · 1 tool call");
    assert_eq!(
        calls_summary_text(2, "bash · read"),
        "… 2 tool calls · bash · read"
    );
    assert_eq!(calls_summary_text(1, ""), "… 1 tool call");
}

#[test]
fn pending_request_decorates_rows_without_materializing_identity_rows() {
    let rows =
        TimelineRows::All(100_000).with_request_boundaries(vec![super::RequestBoundaryPlacement {
            output_row: 100_000,
            request: 7,
            run_index: 0,
            terminal: true,
        }]);
    assert_eq!(rows.len(), 100_001);
    assert_eq!(rows.get(99_999), Some(TimelineLedgerRow::Record(99_999)));
    assert_eq!(
        rows.get(100_000),
        Some(TimelineLedgerRow::RequestBoundary {
            request: 7,
            run_index: 0,
            terminal: true,
        })
    );
    assert_eq!(rows.get(100_001), None);
}

#[test]
fn variable_ledger_list_preserves_and_restores_logical_scroll() {
    let state = gpui_kit::ListState::new(4, gpui_kit::ListAlignment::Top, gpui_kit::px(100.0));
    state.scroll_to(gpui_kit::ListOffset {
        item_ix: 2,
        offset_in_item: gpui_kit::px(7.0),
    });

    sync_trajectory_list_state(&state, 7, false, None, false);
    assert_eq!(state.item_count(), 7);
    assert_eq!(state.logical_scroll_top().item_ix, 2);
    assert_eq!(state.logical_scroll_top().offset_in_item, gpui_kit::px(7.0));

    sync_trajectory_list_state(
        &state,
        3,
        true,
        Some(gpui_kit::ListOffset {
            item_ix: 1,
            offset_in_item: gpui_kit::px(4.0),
        }),
        false,
    );
    assert_eq!(state.item_count(), 3);
    assert_eq!(state.logical_scroll_top().item_ix, 1);
    assert_eq!(state.logical_scroll_top().offset_in_item, gpui_kit::px(4.0));
}

#[test]
fn variable_ledger_list_follows_the_tail_only_when_enabled() {
    let state = gpui_kit::ListState::new(0, gpui_kit::ListAlignment::Top, gpui_kit::px(100.0));
    sync_trajectory_list_state(&state, 4, true, None, true);
    assert_eq!(state.logical_scroll_top().item_ix, 4);

    sync_trajectory_list_state(&state, 7, false, None, true);
    assert_eq!(state.logical_scroll_top().item_ix, 7);

    state.scroll_to(gpui_kit::ListOffset {
        item_ix: 2,
        offset_in_item: gpui_kit::px(3.0),
    });
    sync_trajectory_list_state(&state, 8, false, None, false);
    assert_eq!(state.logical_scroll_top().item_ix, 2);
    assert_eq!(state.logical_scroll_top().offset_in_item, gpui_kit::px(3.0));
}

#[test]
fn timeline_bar_gap_matches_dsh_and_leaves_track_edges_visible() {
    assert_eq!(timeline_bar_gap_px(0.0), 0.0);
    assert_eq!(timeline_bar_gap_px(10.0), 0.8);
    assert_eq!(timeline_bar_gap_px(12.5), 1.0);
    assert_eq!(timeline_bar_gap_px(1_500.0), 1.0);
}

#[test]
fn variable_ledger_scroll_alignment_accounts_for_summary_height() {
    let rows = TimelineRows::Projected(
        [
            TimelineLedgerRow::Record(0),
            TimelineLedgerRow::TurnSummary {
                representative: 0,
                turn: 1,
                first_hidden: 1,
                last_hidden: 1,
                step_ids: ImHashSet::new(),
                call_count: 0,
            },
            TimelineLedgerRow::Record(2),
            TimelineLedgerRow::Record(3),
        ]
        .into_iter()
        .collect(),
    );

    let centered = aligned_trajectory_list_offset(&rows, 3, 80.0, 0.5);
    assert_eq!(centered.item_ix, 2);
    assert_eq!(centered.offset_in_item, gpui_kit::px(5.0));
    let bottom = aligned_trajectory_list_offset(&rows, 3, 80.0, 1.0);
    assert_eq!(bottom.item_ix, 1);
    assert_eq!(bottom.offset_in_item, gpui_kit::px(0.0));
}

#[test]
fn focused_rows_center_small_ranges_and_anchor_large_ranges() {
    let rows = TimelineRows::All(30);
    assert_eq!(
        focus_scroll_target(&[4, 5, 6], &rows, 120.0),
        Some((5, ScrollStrategy::Center))
    );
    assert_eq!(
        focus_scroll_target(&(10..24).collect::<Vec<_>>(), &rows, 300.0),
        Some((10, ScrollStrategy::Top))
    );
}

#[test]
fn ledger_item_inside_a_focused_range_keeps_the_selection() {
    let focused = HashSet::from([4, 5, 6]);
    assert!(!should_clear_selection_for_record(
        TrajectorySelectionSource::Ledger,
        Some(&focused),
        5,
    ));
    assert!(should_clear_selection_for_record(
        TrajectorySelectionSource::Ledger,
        Some(&focused),
        7,
    ));
    assert!(should_clear_selection_for_record(
        TrajectorySelectionSource::Timeline,
        Some(&focused),
        5,
    ));
}
