use super::*;

#[cfg(test)]
pub(super) fn collapsible_trajectory_groups(
    records: &Vector<Arc<TrajectoryRecord>>,
) -> (HashSet<u32>, HashSet<TrajectoryItemId>) {
    let mut content_per_turn = std::collections::HashMap::<u32, usize>::new();
    let mut assistants = HashSet::new();
    for (index, record) in records.iter().enumerate() {
        if record.kind != TrajectoryKind::System
            && let Some(turn) = record.turn
        {
            *content_per_turn.entry(turn).or_default() += 1;
        }
        if record.kind == TrajectoryKind::Assistant
            && records
                .get(index + 1)
                .is_some_and(|next| next.kind == TrajectoryKind::Tool)
        {
            assistants.insert(record.id.clone());
        }
    }
    (
        content_per_turn
            .into_iter()
            .filter_map(|(turn, count)| (count > 1).then_some(turn))
            .collect(),
        assistants,
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TrajectorySelectionSource {
    Ledger,
    Timeline,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum LedgerFoldTarget {
    Turn(u32),
    Assistant(TrajectoryItemId),
}

pub(super) fn ledger_double_click_target(
    record: &TrajectoryRecord,
    turn_start: bool,
    collapsed_turns: &HashSet<u32>,
    collapsible_turns: &HashSet<u32>,
    collapsible_assistants: &HashSet<TrajectoryItemId>,
) -> Option<LedgerFoldTarget> {
    if let Some(turn) = record.turn
        && collapsed_turns.contains(&turn)
    {
        return Some(LedgerFoldTarget::Turn(turn));
    }
    if record.kind == TrajectoryKind::Assistant && collapsible_assistants.contains(&record.id) {
        return Some(LedgerFoldTarget::Assistant(record.id.clone()));
    }
    let turn = record.turn?;
    (turn_start && collapsible_turns.contains(&turn)).then_some(LedgerFoldTarget::Turn(turn))
}

pub(super) fn ledger_row_turn(
    row: &TimelineLedgerRow,
    records: &Vector<Arc<TrajectoryRecord>>,
) -> Option<u32> {
    match row {
        TimelineLedgerRow::Record(index) => records.get(*index).and_then(|record| record.turn),
        TimelineLedgerRow::TurnSummary { turn, .. } => Some(*turn),
        TimelineLedgerRow::CallsSummary { assistant, .. } => {
            records.get(*assistant).and_then(|record| record.turn)
        }
        TimelineLedgerRow::RequestBoundary { .. } => None,
    }
}

pub(super) fn ledger_record_boundaries(
    rows: &TimelineRows,
    row: usize,
    record: &TrajectoryRecord,
    records: &Vector<Arc<TrajectoryRecord>>,
) -> (bool, bool, bool) {
    let Some(turn) = record.turn else {
        return (false, false, false);
    };
    let previous_turn = row
        .checked_sub(1)
        .and_then(|previous| rows.get(previous))
        .and_then(|previous| ledger_row_turn(&previous, records));
    let next_turn = rows
        .get(row + 1)
        .and_then(|next| ledger_row_turn(&next, records));
    (
        previous_turn != Some(turn),
        previous_turn == Some(turn),
        next_turn == Some(turn),
    )
}

#[derive(Debug)]
pub(super) struct TimelineModel {
    pub(super) axis: AxisId,
    pub(super) domain: DomainRange,
    pub(super) viewport: DomainRange,
    pub(super) render_width_px: f64,
    pub(super) cells: Vec<RenderCell>,
}

impl TimelineModel {
    pub(super) fn hit_test(&self, lane: TimelineLane, fraction: f64) -> Option<usize> {
        let x_px = fraction.clamp(0.0, 1.0) * self.render_width_px;
        self.cells.iter().rev().find_map(|cell| {
            let end_px = cell.end_px.max(cell.start_px + 1.0);
            (cell.lane == lane && x_px >= cell.start_px && x_px <= end_px)
                .then(|| cell.ids.last().copied())
                .flatten()
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct TimelineView {
    pub(super) viewport: Option<AxisRange>,
    pub(super) selection: Option<AxisRange>,
    pub(super) render_width_px: f64,
}

#[derive(Clone, Debug)]
pub(super) enum TimelineMatches {
    All(usize),
    Filtered(ImHashSet<usize>),
}

impl TimelineMatches {
    pub(super) fn contains(&self, index: &usize) -> bool {
        match self {
            Self::All(len) => *index < *len,
            Self::Filtered(indices) => indices.contains(index),
        }
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        match self {
            Self::All(len) => *len,
            Self::Filtered(indices) => indices.len(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum TimelineLedgerRow {
    Record(usize),
    /// Presentation-only anchor for the short live interval before a canonical request has a
    /// record result. It deliberately has no timeline/search/fold identity.
    RequestBoundary {
        request: usize,
        /// Consecutive request-only boundaries share a zero-height ledger position in DSH and
        /// fan their markers horizontally so retries remain individually selectable.
        run_index: u16,
        /// Only the final boundary at the document tail reserves nine pixels below its marker.
        terminal: bool,
    },
    TurnSummary {
        representative: usize,
        turn: u32,
        first_hidden: usize,
        last_hidden: usize,
        step_ids: ImHashSet<u32>,
        call_count: usize,
    },
    CallsSummary {
        assistant: usize,
        first_tool: usize,
        last_tool: usize,
        tool_names: ImHashSet<String>,
        tools: Arc<str>,
        tools_truncated: bool,
    },
}

impl TimelineLedgerRow {
    pub(super) fn representative_index(&self) -> usize {
        match self {
            Self::Record(index) => *index,
            Self::RequestBoundary { .. } => usize::MAX,
            Self::TurnSummary { representative, .. } => *representative,
            Self::CallsSummary { assistant, .. } => *assistant,
        }
    }

    pub(super) fn represents(
        &self,
        record_index: usize,
        records: &Vector<Arc<TrajectoryRecord>>,
    ) -> bool {
        match self {
            Self::Record(index) => *index == record_index,
            Self::RequestBoundary { .. } => false,
            Self::TurnSummary {
                turn,
                first_hidden,
                last_hidden,
                ..
            } => {
                (*first_hidden..=*last_hidden).contains(&record_index)
                    && records.get(record_index).is_some_and(|record| {
                        record.turn == Some(*turn) && record.kind != TrajectoryKind::System
                    })
            }
            Self::CallsSummary {
                first_tool,
                last_tool,
                ..
            } => (*first_tool..=*last_tool).contains(&record_index),
        }
    }

    pub(super) fn intersects(
        &self,
        focused: &HashSet<usize>,
        records: &Vector<Arc<TrajectoryRecord>>,
    ) -> bool {
        match self {
            Self::Record(index) => focused.contains(index),
            Self::RequestBoundary { .. } => false,
            Self::TurnSummary {
                first_hidden,
                last_hidden,
                ..
            }
            | Self::CallsSummary {
                first_tool: first_hidden,
                last_tool: last_hidden,
                ..
            } => (*first_hidden..=*last_hidden)
                .any(|index| focused.contains(&index) && self.represents(index, records)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum TimelineRows {
    /// Identity mapping: row N is record N. The overwhelmingly common empty-search/default-filter
    /// state therefore owns no N-element allocation.
    All(usize),
    Projected(Vector<TimelineLedgerRow>),
    WithRequestBoundaries {
        base: Box<TimelineRows>,
        boundaries: Arc<[RequestBoundaryPlacement]>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct RequestBoundaryPlacement {
    pub(super) output_row: usize,
    pub(super) request: usize,
    pub(super) run_index: u16,
    pub(super) terminal: bool,
}

/// Tail cursor for append-only folded projections. A collapsed turn can contain explicit System
/// rows after its summary, so searching backward from the ledger tail is quadratic for alternating
/// System/content streams. Stable row positions make every append independent of prior length.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct FoldAppendState {
    pub(super) tail_turn: Option<u32>,
    pub(super) first_content: Option<usize>,
    pub(super) first_content_row: Option<usize>,
    pub(super) turn_summary_row: Option<usize>,
}

impl FoldAppendState {
    pub(super) fn from_projection(
        records: &Vector<Arc<TrajectoryRecord>>,
        rows: &TimelineRows,
        collapsed_turns: &HashSet<u32>,
    ) -> Self {
        let Some(last_index) = records.len().checked_sub(1) else {
            return Self::default();
        };
        let Some(turn) = records[last_index].turn else {
            return Self::default();
        };
        if !collapsed_turns.contains(&turn) {
            return Self::default();
        }
        let mut group_start = last_index;
        while group_start > 0 && records[group_start - 1].turn == Some(turn) {
            group_start -= 1;
        }
        let first_content =
            (group_start..=last_index).find(|index| records[*index].kind != TrajectoryKind::System);
        let first_content_row = first_content.and_then(|first| {
            rows.position(|row| matches!(row, TimelineLedgerRow::Record(index) if *index == first))
        });
        let turn_summary_row = first_content_row.and_then(|row| {
            rows.get(row + 1).and_then(|candidate| {
                matches!(candidate, TimelineLedgerRow::TurnSummary { turn: row_turn, .. } if row_turn == turn)
                    .then_some(row + 1)
            })
        });
        Self {
            tail_turn: Some(turn),
            first_content,
            first_content_row,
            turn_summary_row,
        }
    }
}

impl TimelineRows {
    pub(super) fn len(&self) -> usize {
        match self {
            Self::All(len) => *len,
            Self::Projected(rows) => rows.len(),
            Self::WithRequestBoundaries { base, boundaries } => {
                base.len().saturating_add(boundaries.len())
            }
        }
    }

    pub(super) fn get(&self, row: usize) -> Option<TimelineLedgerRow> {
        match self {
            Self::All(len) => (row < *len).then_some(TimelineLedgerRow::Record(row)),
            Self::Projected(rows) => rows.get(row).cloned(),
            Self::WithRequestBoundaries { base, boundaries } => {
                match boundaries.binary_search_by_key(&row, |boundary| boundary.output_row) {
                    Ok(index) => {
                        boundaries
                            .get(index)
                            .map(|boundary| TimelineLedgerRow::RequestBoundary {
                                request: boundary.request,
                                run_index: boundary.run_index,
                                terminal: boundary.terminal,
                            })
                    }
                    Err(boundaries_before) => base.get(row.saturating_sub(boundaries_before)),
                }
            }
        }
    }

    pub(super) fn position(
        &self,
        mut predicate: impl FnMut(&TimelineLedgerRow) -> bool,
    ) -> Option<usize> {
        (0..self.len()).find(|row| self.get(*row).as_ref().is_some_and(&mut predicate))
    }

    pub(super) fn with_request_boundaries(self, boundaries: Vec<RequestBoundaryPlacement>) -> Self {
        if boundaries.is_empty() {
            return self;
        }
        Self::WithRequestBoundaries {
            base: Box::new(self),
            boundaries: boundaries.into(),
        }
    }

    pub(super) fn shares_structure(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::All(left), Self::All(right)) => left == right,
            (Self::Projected(left), Self::Projected(right)) => left.ptr_eq(right),
            _ => false,
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct TimelineFilterSnapshot {
    pub(super) matched_cells: TimelineCellMatches,
    pub(super) rows: TimelineRows,
    pub(super) fold_controls: TimelineFoldControlSnapshot,
}

#[derive(Clone, Debug, Default)]
pub(super) struct TimelineFoldControlSnapshot {
    pub(super) all_turns_collapsed: bool,
    pub(super) all_assistants_collapsed: bool,
}

#[derive(Clone, Debug)]
pub(super) struct TimelineFoldControlCache {
    pub(super) projection_lineage: u64,
    pub(super) eligibility_revision: u64,
    pub(super) state_revision: u64,
    pub(super) snapshot: TimelineFoldControlSnapshot,
}

pub(super) struct TimelinePaintContext<'a> {
    pub(super) render_width_px: f64,
    pub(super) focused_cells: Option<&'a HashSet<usize>>,
    pub(super) matching: &'a TimelineCellMatches,
    pub(super) hovered_cell: Option<usize>,
    pub(super) selected_cell: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum InspectorTarget {
    Record(usize),
    Request(usize),
}

#[derive(Clone, Debug)]
pub(super) enum TimelineCellMatches {
    All,
    Filtered(ImHashSet<usize>),
}

impl TimelineCellMatches {
    pub(super) fn contains(&self, cell: usize) -> bool {
        match self {
            Self::All => true,
            Self::Filtered(cells) => cells.contains(&cell),
        }
    }
}

#[derive(Debug)]
pub(super) struct TimelineSearchCache {
    pub(super) change_revision: u64,
    pub(super) fold_state_revision: u64,
    pub(super) fold_eligibility_revision: u64,
    pub(super) query: String,
    pub(super) terms: Arc<[String]>,
    pub(super) matching_indices: TimelineMatches,
    pub(super) rows: TimelineRows,
    pub(super) fold_append: FoldAppendState,
    pub(super) collapsed_turns: HashSet<u32>,
    pub(super) collapsed_assistants: HashSet<TrajectoryItemId>,
    pub(super) record_count: usize,
    pub(super) matched_cells: TimelineCellMatches,
    pub(super) matched_model_revision: u64,
    #[cfg(test)]
    pub(super) inspected_records: usize,
    #[cfg(test)]
    pub(super) materialized_row_rebuilds: usize,
    #[cfg(test)]
    pub(super) matched_cell_rescans: usize,
}

pub(super) struct TimelineSearchBuild<'a> {
    pub(super) change_revision: u64,
    pub(super) query: &'a str,
    pub(super) fold_state_revision: u64,
    pub(super) fold_eligibility_revision: u64,
    pub(super) collapsed_turns: &'a HashSet<u32>,
    pub(super) collapsed_assistants: &'a HashSet<TrajectoryItemId>,
}

#[derive(Clone, Debug)]
pub(super) struct TimelineFocusCache {
    pub(super) selection: AxisRange,
    pub(super) record_indices: Arc<HashSet<usize>>,
    pub(super) focused_cells: Arc<HashSet<usize>>,
    pub(super) model_revision: u64,
    pub(super) focused_prefix: Arc<[usize]>,
    pub(super) focused_non_system_prefix: Arc<[usize]>,
}

impl TimelineFocusCache {
    pub(super) fn new(
        selection: AxisRange,
        records: &Vector<Arc<TrajectoryRecord>>,
        record_indices: HashSet<usize>,
    ) -> Self {
        let mut focused_prefix = Vec::with_capacity(records.len() + 1);
        let mut focused_non_system_prefix = Vec::with_capacity(records.len() + 1);
        focused_prefix.push(0);
        focused_non_system_prefix.push(0);
        for (index, record) in records.iter().enumerate() {
            let focused = usize::from(record_indices.contains(&index));
            focused_prefix.push(focused_prefix.last().copied().unwrap_or_default() + focused);
            focused_non_system_prefix.push(
                focused_non_system_prefix
                    .last()
                    .copied()
                    .unwrap_or_default()
                    + usize::from(focused != 0 && record.kind != TrajectoryKind::System),
            );
        }
        Self {
            selection,
            record_indices: Arc::new(record_indices),
            focused_cells: Arc::new(HashSet::new()),
            model_revision: u64::MAX,
            focused_prefix: focused_prefix.into(),
            focused_non_system_prefix: focused_non_system_prefix.into(),
        }
    }

    pub(super) fn sync_model(
        &mut self,
        geometry: &TimelineGeometry,
        model: &TimelineModel,
        model_revision: u64,
    ) {
        if self.model_revision == model_revision {
            return;
        }
        let visible_members = model.cells.iter().map(|cell| cell.ids.len()).sum::<usize>();
        let mut focused_cells = HashSet::new();
        if self.record_indices.len().saturating_mul(8) <= visible_members {
            for record in self.record_indices.iter() {
                if let Some(cell) = geometry.render_cell_for_record(&model.cells, *record) {
                    focused_cells.insert(cell);
                }
            }
        } else {
            for (ordinal, cell) in model.cells.iter().enumerate() {
                if geometry
                    .render_members(cell)
                    .any(|record| self.record_indices.contains(record))
                {
                    focused_cells.insert(ordinal);
                }
            }
        }
        self.focused_cells = Arc::new(focused_cells);
        self.model_revision = model_revision;
    }

    pub(super) fn intersects(&self, first: usize, last: usize) -> bool {
        Self::prefix_intersects(&self.focused_prefix, first, last)
    }

    pub(super) fn intersects_non_system(&self, first: usize, last: usize) -> bool {
        Self::prefix_intersects(&self.focused_non_system_prefix, first, last)
    }

    pub(super) fn prefix_intersects(prefix: &[usize], first: usize, last: usize) -> bool {
        if first > last || first >= prefix.len().saturating_sub(1) {
            return false;
        }
        let end = last.saturating_add(1).min(prefix.len().saturating_sub(1));
        prefix[end] > prefix[first]
    }
}

#[derive(Debug)]
pub(super) struct TimelineCacheIdentity {
    pub(super) axis: AxisId,
    pub(super) change_revision: u64,
}

#[derive(Debug)]
pub(super) struct RequestRowsCache {
    pub(super) boundary_revision: u64,
    pub(super) base: TimelineRows,
    pub(super) rows: TimelineRows,
}

#[derive(Debug)]
pub(crate) struct TimelineModelCache {
    pub(super) change_revision: u64,
    pub(super) record_count: usize,
    pub(super) viewport: Option<AxisRange>,
    pub(super) render_width_px: f64,
    pub(super) geometry: Option<TimelineGeometry>,
    pub(super) search: Option<TimelineSearchCache>,
    pub(super) request_rows: Option<RequestRowsCache>,
    pub(super) fold_controls: Option<TimelineFoldControlCache>,
    pub(super) model: Option<TimelineModel>,
    pub(super) model_revision: u64,
    pub(super) focus: Option<TimelineFocusCache>,
    #[cfg(test)]
    pub(super) timed_incremental_updates: usize,
}

impl TimelineModelCache {
    pub(super) fn new(
        identity: TimelineCacheIdentity,
        records: &Vector<std::sync::Arc<TrajectoryRecord>>,
        view: TimelineView,
        mut search: Option<TimelineSearchCache>,
    ) -> Self {
        let TimelineCacheIdentity {
            axis,
            change_revision,
        } = identity;
        let geometry = timeline_geometry_from_iter(records.iter(), axis);
        // `model_revision` is local to one cache instance. A retained content/search cache must
        // never mistake a freshly built LOD model for the prior cache's model with the same local
        // counter value.
        if let Some(search) = &mut search {
            search.matched_model_revision = u64::MAX;
        }
        let mut cache = Self {
            change_revision,
            record_count: records.len(),
            viewport: view.viewport,
            render_width_px: view.render_width_px,
            geometry,
            search,
            request_rows: None,
            fold_controls: None,
            model: None,
            model_revision: 0,
            focus: None,
            #[cfg(test)]
            timed_incremental_updates: 0,
        };
        cache.reproject(cache.resolved_viewport(view.viewport));
        cache.sync_focus(records, view.selection, true);
        cache
    }

    pub(super) fn geometry_matches(&self, axis: AxisId) -> bool {
        self.geometry
            .as_ref()
            .is_some_and(|geometry| geometry.axis == axis)
    }

    pub(super) fn projection_matches(&self, document_generation: u64, mode: TimelineMode) -> bool {
        self.geometry.as_ref().is_some_and(|geometry| {
            geometry.axis.document_generation == document_generation && geometry.axis.mode == mode
        })
    }

    pub(super) fn sync_ranges(&mut self, viewport: Option<AxisRange>, render_width_px: f64) {
        let projection_changed =
            self.viewport != viewport || (self.render_width_px - render_width_px).abs() >= 1.0;
        self.viewport = viewport;
        self.render_width_px = render_width_px;
        if projection_changed {
            self.reproject(self.resolved_viewport(viewport));
        }
    }

    pub(super) fn sync_sequence_geometry(
        &mut self,
        projection: &crate::session::TrajectoryProjection,
        changes: crate::session::TrajectoryChanges,
    ) -> bool {
        if self
            .geometry
            .as_ref()
            .is_none_or(|geometry| geometry.axis.mode != TimelineMode::Sequence)
        {
            return false;
        }
        let document_generation = self
            .geometry
            .as_ref()
            .map(|geometry| geometry.axis.document_generation)
            .unwrap_or_default();
        let axis = AxisId {
            document_generation,
            geometry_revision: projection.revision(),
            mode: TimelineMode::Sequence,
        };
        let Some(changed_indices) = changes.geometry_indices() else {
            return false;
        };
        let changed_indices = changed_indices.collect::<Vector<_>>();
        let spans = changed_indices.iter().filter_map(|index| {
            projection
                .records
                .get(*index)
                .map(|record| (*index, timeline_span(*index, record.as_ref())))
        });
        let Some(geometry) = &mut self.geometry else {
            return false;
        };
        let previous_domain = geometry.domain;
        let previous_viewport = self
            .model
            .as_ref()
            .map_or(previous_domain, |model| model.viewport);
        let appended = projection.records.len() > self.record_count;
        self.record_count = projection.records.len();
        if !geometry.update_sequence(axis, projection.records.len(), spans) {
            return false;
        }
        self.change_revision = changes.revision;
        let viewport = if appended && previous_viewport == previous_domain {
            geometry.domain
        } else {
            previous_viewport.clamp_to(geometry.domain)
        };
        if appended {
            // Appending changes the Sequence domain and potentially every projected x position.
            // This is the only compatible Sequence transition that still needs a full LOD pass.
            self.reproject(viewport);
        } else {
            self.update_sequence_model_cells(&changed_indices);
        }
        true
    }

    pub(super) fn sync_timed_geometry(
        &mut self,
        projection: &crate::session::TrajectoryProjection,
        changes: crate::session::TrajectoryChanges,
    ) -> bool {
        let Some(mode @ (TimelineMode::Duration | TimelineMode::Actual)) =
            self.geometry.as_ref().map(|geometry| geometry.axis.mode)
        else {
            return false;
        };
        let document_generation = self
            .geometry
            .as_ref()
            .map(|geometry| geometry.axis.document_generation)
            .unwrap_or_default();
        let axis = AxisId {
            document_generation,
            geometry_revision: projection.revision(),
            mode,
        };
        let Some(changed_indices) = changes.geometry_indices() else {
            return false;
        };
        let spans = changed_indices.filter_map(|index| {
            projection
                .records
                .get(index)
                .map(|record| (index, timeline_span(index, record.as_ref())))
        });
        let Some(geometry) = &mut self.geometry else {
            return false;
        };
        let previous_domain = geometry.domain;
        let previous_viewport = self
            .model
            .as_ref()
            .map_or(previous_domain, |model| model.viewport);
        if geometry
            .update_timed(axis, projection.records.len(), spans)
            .is_none()
        {
            return false;
        }
        self.record_count = projection.records.len();
        self.change_revision = changes.revision;
        #[cfg(test)]
        {
            self.timed_incremental_updates = self.timed_incremental_updates.saturating_add(1);
        }
        let viewport = if previous_viewport == previous_domain {
            geometry.domain
        } else {
            previous_viewport.clamp_to(geometry.domain)
        };
        self.reproject(viewport);
        true
    }

    /// Consumes the shared bounded change journal even when geometry did not change. This keeps
    /// text-only streaming receipts from aging the geometry cursor out of the journal and returns
    /// whether selection focus depends on any consumed change.
    pub(super) fn sync_projection(
        &mut self,
        projection: &crate::session::TrajectoryProjection,
    ) -> Option<bool> {
        let changes = projection.changes_since(self.change_revision)?;
        let geometry_changed = self
            .geometry
            .as_ref()
            .is_none_or(|geometry| geometry.axis.geometry_revision != projection.revision());
        let focus_changed = geometry_changed || changes.search_indices().is_none();
        if geometry_changed {
            let updated = match self.geometry.as_ref().map(|geometry| geometry.axis.mode) {
                Some(TimelineMode::Sequence) => self.sync_sequence_geometry(projection, changes),
                Some(TimelineMode::Duration | TimelineMode::Actual) => {
                    self.sync_timed_geometry(projection, changes)
                }
                None => false,
            };
            updated.then_some(focus_changed)
        } else {
            self.change_revision = changes.revision;
            Some(focus_changed)
        }
    }

    pub(super) fn update_sequence_model_cells(&mut self, changed_indices: &Vector<usize>) {
        let Some(geometry) = &self.geometry else {
            return;
        };
        let Some(model) = &mut self.model else {
            return;
        };
        model.axis = geometry.axis;
        model.domain = geometry.domain;
        model.viewport = model.viewport.clamp_to(geometry.domain);
        for index in changed_indices {
            let Some(cell_index) = geometry.render_cell_for_record(&model.cells, *index) else {
                continue;
            };
            let Some(cell) = model.cells.get_mut(cell_index) else {
                continue;
            };
            if cell.clustered {
                continue;
            }
            cell.nested = geometry
                .cells
                .get(*index)
                .and_then(|geometry_cell| geometry_cell.nested)
                .map(|range| {
                    let (left, width) = normalized_range(range, model.viewport);
                    (
                        left * model.render_width_px,
                        (left + width) * model.render_width_px,
                    )
                });
        }
    }

    pub(super) fn reproject(&mut self, viewport: DomainRange) {
        self.model = self.geometry.as_ref().map(|geometry| {
            project_timeline(geometry, viewport, self.render_width_px, self.record_count)
        });
        self.model_revision = self.model_revision.saturating_add(1);
    }

    pub(super) fn display_selection(&self, selection: Option<AxisRange>) -> Option<AxisRange> {
        self.geometry
            .as_ref()
            .and_then(|geometry| resolved_axis_range(selection, geometry))
    }

    pub(super) fn resolved_viewport(&self, viewport: Option<AxisRange>) -> DomainRange {
        let Some(geometry) = &self.geometry else {
            return DomainRange::new(0.0, 1.0);
        };
        resolved_axis_range(viewport, geometry).map_or(geometry.domain, |range| range.range)
    }

    #[allow(
        clippy::expect_used,
        reason = "request row cache is initialized by the preceding branch"
    )]
    pub(super) fn request_rows(
        &mut self,
        projection: &crate::session::TrajectoryProjection,
        base: TimelineRows,
    ) -> TimelineRows {
        let boundary_revision = projection.request_boundary_revision();
        let reuse = self.request_rows.as_ref().is_some_and(|cache| {
            cache.boundary_revision == boundary_revision && cache.base.shares_structure(&base)
        });
        if !reuse {
            let boundaries = request_boundary_placements(&base, projection);
            let rows = base.clone().with_request_boundaries(boundaries);
            self.request_rows = Some(RequestRowsCache {
                boundary_revision,
                base,
                rows,
            });
        }
        self.request_rows
            .as_ref()
            .expect("request rows were initialized")
            .rows
            .clone()
    }

    #[allow(
        clippy::expect_used,
        reason = "search and fold caches are checked or initialized before access"
    )]
    pub(super) fn search_snapshot(
        &mut self,
        projection: &crate::session::TrajectoryProjection,
        query: &str,
        fold_state_revision: u64,
        collapsed_turns: &HashSet<u32>,
        collapsed_assistants: &HashSet<TrajectoryItemId>,
    ) -> TimelineFilterSnapshot {
        let query_active = query.split_whitespace().next().is_some();
        let rebuild = self.search.as_ref().is_none_or(|search| {
            search.query != query
                || (!query_active
                    && (search.fold_state_revision != fold_state_revision
                        || search.fold_eligibility_revision
                            != projection.fold_eligibility_revision()))
                || projection
                    .changes_since(search.change_revision)
                    .is_none_or(|changes| changes.search_indices().is_none())
        });
        let changed = if rebuild {
            self.search = Some(TimelineSearchCache::build(
                projection,
                query,
                fold_state_revision,
                collapsed_turns,
                collapsed_assistants,
            ));
            Vector::new()
        } else {
            self.search
                .as_mut()
                .expect("timeline search cache was checked above")
                .sync_incremental(projection)
        };
        if query_active
            && let Some(search) = &mut self.search
            && search.fold_state_revision != fold_state_revision
        {
            // Search deliberately bypasses folds. Remember the revision without rebuilding the
            // N-record match projection; the canonical folds will be applied when search clears.
            search.fold_state_revision = fold_state_revision;
            search.collapsed_turns = collapsed_turns.clone();
            search.collapsed_assistants = collapsed_assistants.clone();
        }
        let (matched_cells, base_rows) = {
            let search = self
                .search
                .as_mut()
                .expect("timeline search cache was initialized");
            if let (Some(geometry), Some(model)) = (&self.geometry, &self.model) {
                search.sync_model_matches(geometry, model, self.model_revision, &changed);
            }
            (search.matched_cells.clone(), search.rows.clone())
        };
        let rows = if query_active {
            // DSH deliberately excludes request-only presentation boundaries from search.
            base_rows
        } else {
            self.request_rows(projection, base_rows)
        };
        let projection_lineage = projection.projection_lineage();
        let eligibility_revision = projection.fold_eligibility_revision();
        let refresh_fold_controls = self.fold_controls.as_ref().is_none_or(|cache| {
            cache.projection_lineage != projection_lineage
                || cache.eligibility_revision != eligibility_revision
                || cache.state_revision != fold_state_revision
        });
        if refresh_fold_controls {
            let turns = projection.collapsible_turns();
            let assistants = projection.collapsible_assistants();
            self.fold_controls = Some(TimelineFoldControlCache {
                projection_lineage,
                eligibility_revision,
                state_revision: fold_state_revision,
                snapshot: TimelineFoldControlSnapshot {
                    all_turns_collapsed: !turns.is_empty()
                        && turns.iter().all(|turn| collapsed_turns.contains(turn)),
                    all_assistants_collapsed: !assistants.is_empty()
                        && assistants
                            .iter()
                            .all(|assistant| collapsed_assistants.contains(assistant)),
                },
            });
        }
        TimelineFilterSnapshot {
            matched_cells,
            rows,
            fold_controls: self
                .fold_controls
                .as_ref()
                .expect("fold controls were initialized")
                .snapshot
                .clone(),
        }
    }

    pub(super) fn sync_focus(
        &mut self,
        records: &Vector<Arc<TrajectoryRecord>>,
        selection: Option<AxisRange>,
        force: bool,
    ) {
        let Some(selection) = self.display_selection(selection) else {
            self.focus = None;
            return;
        };
        let Some(geometry) = &self.geometry else {
            self.focus = None;
            return;
        };
        let Some(model) = &self.model else {
            self.focus = None;
            return;
        };
        if !force
            && let Some(focus) = self
                .focus
                .as_mut()
                .filter(|focus| focus.selection == selection)
        {
            focus.sync_model(geometry, model, self.model_revision);
            return;
        }
        let items = geometry.selection(selection).items;
        let mut record_indices = HashSet::with_capacity(items.len());
        for index in items {
            record_indices.insert(index);
        }
        let mut focus = TimelineFocusCache::new(selection, records, record_indices);
        focus.sync_model(geometry, model, self.model_revision);
        self.focus = Some(focus);
    }
}

impl TimelineSearchCache {
    pub(super) fn build(
        projection: &crate::session::TrajectoryProjection,
        query: &str,
        fold_state_revision: u64,
        collapsed_turns: &HashSet<u32>,
        collapsed_assistants: &HashSet<TrajectoryItemId>,
    ) -> Self {
        let eligible_turns = projection.collapsible_turns();
        let eligible_assistants = projection.collapsible_assistants();
        let effective_turns = collapsed_turns
            .iter()
            .filter(|turn| eligible_turns.contains(turn))
            .copied()
            .collect::<HashSet<_>>();
        let effective_assistants = collapsed_assistants
            .iter()
            .filter(|assistant| eligible_assistants.contains(*assistant))
            .cloned()
            .collect::<HashSet<_>>();
        Self::build_records_with_folds_using(
            &projection.records,
            TimelineSearchBuild {
                change_revision: projection.change_revision(),
                query,
                fold_state_revision,
                fold_eligibility_revision: projection.fold_eligibility_revision(),
                collapsed_turns: &effective_turns,
                collapsed_assistants: &effective_assistants,
            },
            |index, _, terms| projection.record_matches_terms(index, terms),
        )
    }

    #[cfg(test)]
    pub(super) fn build_records_with_folds(
        records: &Vector<Arc<TrajectoryRecord>>,
        change_revision: u64,
        query: &str,
        fold_state_revision: u64,
        fold_eligibility_revision: u64,
        collapsed_turns: &HashSet<u32>,
        collapsed_assistants: &HashSet<TrajectoryItemId>,
    ) -> Self {
        Self::build_records_with_folds_using(
            records,
            TimelineSearchBuild {
                change_revision,
                query,
                fold_state_revision,
                fold_eligibility_revision,
                collapsed_turns,
                collapsed_assistants,
            },
            |_, record, terms| record.matches_terms(terms),
        )
    }

    pub(super) fn build_records_with_folds_using(
        records: &Vector<Arc<TrajectoryRecord>>,
        build: TimelineSearchBuild<'_>,
        mut matches: impl FnMut(usize, &TrajectoryRecord, &[String]) -> bool,
    ) -> Self {
        let TimelineSearchBuild {
            change_revision,
            query,
            fold_state_revision,
            fold_eligibility_revision,
            collapsed_turns,
            collapsed_assistants,
        } = build;
        let record_count = records.len();
        let terms: Arc<[String]> = query
            .split_whitespace()
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>()
            .into();
        let query_active = !terms.is_empty();
        if !query_active && collapsed_turns.is_empty() && collapsed_assistants.is_empty() {
            return Self {
                change_revision,
                fold_state_revision,
                fold_eligibility_revision,
                query: String::new(),
                terms,
                matching_indices: TimelineMatches::All(record_count),
                rows: TimelineRows::All(record_count),
                fold_append: FoldAppendState::default(),
                collapsed_turns: HashSet::new(),
                collapsed_assistants: HashSet::new(),
                record_count,
                matched_cells: TimelineCellMatches::All,
                matched_model_revision: 0,
                #[cfg(test)]
                inspected_records: 0,
                #[cfg(test)]
                materialized_row_rebuilds: 0,
                #[cfg(test)]
                matched_cell_rescans: 0,
            };
        }

        let matching_indices = if !query_active {
            TimelineMatches::All(record_count)
        } else {
            let mut matching = ImHashSet::new();
            for (index, record) in records.iter().enumerate() {
                if matches(index, record, &terms) {
                    matching.insert(index);
                }
            }
            TimelineMatches::Filtered(matching)
        };
        let rows = project_ledger_rows(
            records,
            &matching_indices,
            query_active,
            collapsed_turns,
            collapsed_assistants,
        );
        let fold_append = FoldAppendState::from_projection(records, &rows, collapsed_turns);
        Self {
            change_revision,
            fold_state_revision,
            fold_eligibility_revision,
            query: if query_active {
                query.to_owned()
            } else {
                String::new()
            },
            terms,
            matching_indices,
            rows,
            fold_append,
            collapsed_turns: collapsed_turns.clone(),
            collapsed_assistants: collapsed_assistants.clone(),
            record_count,
            matched_cells: TimelineCellMatches::Filtered(ImHashSet::new()),
            matched_model_revision: 0,
            #[cfg(test)]
            inspected_records: if query_active { record_count } else { 0 },
            #[cfg(test)]
            materialized_row_rebuilds: 1,
            #[cfg(test)]
            matched_cell_rescans: 0,
        }
    }

    #[cfg(test)]
    pub(super) fn build_records(
        records: &Vector<Arc<TrajectoryRecord>>,
        change_revision: u64,
        query: &str,
        collapse_turns: bool,
        collapse_calls: bool,
    ) -> Self {
        let (turns, assistants) = collapsible_trajectory_groups(records);
        let turns = if collapse_turns {
            turns
        } else {
            HashSet::new()
        };
        let assistants = if collapse_calls {
            assistants
        } else {
            HashSet::new()
        };
        Self::build_records_with_folds(records, change_revision, query, 0, 0, &turns, &assistants)
    }

    pub(super) fn sync_incremental(
        &mut self,
        projection: &crate::session::TrajectoryProjection,
    ) -> Vector<usize> {
        if self.change_revision == projection.change_revision() {
            self.record_count = projection.records.len();
            if matches!(self.matching_indices, TimelineMatches::All(_)) {
                self.matching_indices = TimelineMatches::All(self.record_count);
            }
            if matches!(self.rows, TimelineRows::All(_)) {
                self.rows = TimelineRows::All(self.record_count);
            }
            return Vector::new();
        }
        let Some(changes) = projection.changes_since(self.change_revision) else {
            let query = self.query.clone();
            let turns = self.collapsed_turns.clone();
            let assistants = self.collapsed_assistants.clone();
            *self = Self::build(
                projection,
                &query,
                self.fold_state_revision,
                &turns,
                &assistants,
            );
            return Vector::new();
        };
        let Some(changed) = changes.search_indices() else {
            let query = self.query.clone();
            let turns = self.collapsed_turns.clone();
            let assistants = self.collapsed_assistants.clone();
            *self = Self::build(
                projection,
                &query,
                self.fold_state_revision,
                &turns,
                &assistants,
            );
            return Vector::new();
        };
        self.sync_changed_records_using(
            &projection.records,
            changes.revision,
            changed,
            |index, _, terms| projection.record_matches_terms(index, terms),
        )
    }

    #[cfg(test)]
    pub(super) fn sync_changed_records(
        &mut self,
        records: &Vector<Arc<TrajectoryRecord>>,
        change_revision: u64,
        changed: impl IntoIterator<Item = usize>,
    ) -> Vector<usize> {
        self.sync_changed_records_using(records, change_revision, changed, |_, record, terms| {
            record.matches_terms(terms)
        })
    }

    pub(super) fn sync_changed_records_using(
        &mut self,
        records: &Vector<Arc<TrajectoryRecord>>,
        change_revision: u64,
        changed: impl IntoIterator<Item = usize>,
        mut matches: impl FnMut(usize, &TrajectoryRecord, &[String]) -> bool,
    ) -> Vector<usize> {
        if self.query.is_empty()
            && self.collapsed_turns.is_empty()
            && self.collapsed_assistants.is_empty()
        {
            self.record_count = records.len();
            self.matching_indices = TimelineMatches::All(self.record_count);
            self.rows = TimelineRows::All(self.record_count);
            self.change_revision = change_revision;
            self.matched_cells = TimelineCellMatches::All;
            return Vector::new();
        }
        if self.query.is_empty() {
            self.matching_indices = TimelineMatches::All(records.len());
        }
        let mut changed_matches = Vector::new();
        let mut visited = HashSet::new();
        let structure_changed = records.len() != self.record_count;
        for index in changed {
            if !visited.insert(index) {
                continue;
            }
            let appended = index >= self.record_count;
            // Empty search matches every record. Existing streaming text changes cannot affect
            // either matching or row filtering, so only newly appended records are inspected.
            if self.query.is_empty() && !appended {
                continue;
            }
            let Some(record) = records.get(index) else {
                *self = Self::build_records_with_folds_using(
                    records,
                    TimelineSearchBuild {
                        change_revision,
                        query: &self.query,
                        fold_state_revision: self.fold_state_revision,
                        fold_eligibility_revision: self.fold_eligibility_revision,
                        collapsed_turns: &self.collapsed_turns,
                        collapsed_assistants: &self.collapsed_assistants,
                    },
                    &mut matches,
                );
                return Vector::new();
            };
            #[cfg(test)]
            {
                self.inspected_records = self.inspected_records.saturating_add(1);
            }
            let matched_before = self.matching_indices.contains(&index);
            if let TimelineMatches::Filtered(indices) = &mut self.matching_indices {
                if matches(index, record, &self.terms) {
                    indices.insert(index);
                } else {
                    indices.remove(&index);
                }
            }
            let matched_after = self.matching_indices.contains(&index);
            if matched_before != matched_after {
                changed_matches.push_back(index);
                if let TimelineRows::Projected(rows) = &mut self.rows {
                    if matched_after {
                        sorted_insert_record(rows, index);
                    } else {
                        sorted_remove_record(rows, index);
                    }
                }
            }
        }
        if self.query.is_empty() && structure_changed {
            if records.len() < self.record_count {
                self.rows = project_ledger_rows(
                    records,
                    &self.matching_indices,
                    false,
                    &self.collapsed_turns,
                    &self.collapsed_assistants,
                );
                self.fold_append =
                    FoldAppendState::from_projection(records, &self.rows, &self.collapsed_turns);
                #[cfg(test)]
                {
                    self.materialized_row_rebuilds =
                        self.materialized_row_rebuilds.saturating_add(1);
                }
            } else if let TimelineRows::Projected(rows) = &mut self.rows {
                for index in self.record_count..records.len() {
                    append_projected_record(
                        records,
                        index,
                        &self.collapsed_turns,
                        &self.collapsed_assistants,
                        rows,
                        &mut self.fold_append,
                    );
                }
            }
        }
        self.record_count = records.len();
        if matches!(self.matching_indices, TimelineMatches::All(_)) {
            self.matching_indices = TimelineMatches::All(self.record_count);
        }
        if matches!(self.rows, TimelineRows::All(_)) {
            self.rows = TimelineRows::All(self.record_count);
        }
        self.change_revision = change_revision;
        changed_matches
    }

    pub(super) fn sync_model_matches(
        &mut self,
        geometry: &TimelineGeometry,
        model: &TimelineModel,
        model_revision: u64,
        changed_matches: &Vector<usize>,
    ) {
        if matches!(self.matching_indices, TimelineMatches::All(_)) {
            self.matched_cells = TimelineCellMatches::All;
            self.matched_model_revision = model_revision;
            return;
        }
        if self.matched_model_revision != model_revision {
            let mut cells = ImHashSet::new();
            if let TimelineMatches::Filtered(indices) = &self.matching_indices {
                for index in indices {
                    let Some(cell) = geometry.render_cell_for_record(&model.cells, *index) else {
                        continue;
                    };
                    cells.insert(cell);
                }
            }
            self.matched_cells = TimelineCellMatches::Filtered(cells);
            self.matched_model_revision = model_revision;
            return;
        }
        let TimelineCellMatches::Filtered(cells) = &mut self.matched_cells else {
            return;
        };
        let affected_cells = changed_matches
            .iter()
            .filter_map(|index| geometry.render_cell_for_record(&model.cells, *index))
            .collect::<HashSet<_>>();
        for cell in affected_cells {
            #[cfg(test)]
            {
                self.matched_cell_rescans = self.matched_cell_rescans.saturating_add(1);
            }
            let matched = model.cells.get(cell).is_some_and(|cell| {
                geometry
                    .render_members(cell)
                    .any(|index| self.matching_indices.contains(index))
            });
            if matched {
                cells.insert(cell);
            } else {
                cells.remove(&cell);
            }
        }
    }
}

pub(super) fn ledger_row_source_seq(
    row: &TimelineLedgerRow,
    records: &Vector<Arc<TrajectoryRecord>>,
) -> Option<u64> {
    let index = match row {
        TimelineLedgerRow::Record(index) => *index,
        TimelineLedgerRow::TurnSummary { representative, .. } => *representative,
        TimelineLedgerRow::CallsSummary { assistant, .. } => *assistant,
        TimelineLedgerRow::RequestBoundary { .. } => return None,
    };
    records.get(index).map(|record| record.source_seq)
}

pub(super) fn request_boundary_placements(
    base: &TimelineRows,
    projection: &crate::session::TrajectoryProjection,
) -> Vec<RequestBoundaryPlacement> {
    let mut pending = projection.unanchored_requests.iter().peekable();
    let mut boundaries = Vec::with_capacity(projection.unanchored_requests.len());
    let mut output_row = 0_usize;

    for base_row in 0..base.len() {
        let mut run_index = 0_usize;
        let next_source = base
            .get(base_row)
            .as_ref()
            .and_then(|row| ledger_row_source_seq(row, &projection.records));
        while let Some(request) = pending.peek()
            && next_source.is_some_and(|source| request.source_seq < source)
        {
            boundaries.push(RequestBoundaryPlacement {
                output_row,
                request: request.request_index,
                run_index: u16::try_from(run_index).unwrap_or(u16::MAX),
                terminal: false,
            });
            pending.next();
            output_row = output_row.saturating_add(1);
            run_index = run_index.saturating_add(1);
        }
        output_row = output_row.saturating_add(1);
    }

    let mut run_index = 0_usize;
    while let Some(request) = pending.next() {
        boundaries.push(RequestBoundaryPlacement {
            output_row,
            request: request.request_index,
            run_index: u16::try_from(run_index).unwrap_or(u16::MAX),
            terminal: pending.peek().is_none(),
        });
        output_row = output_row.saturating_add(1);
        run_index = run_index.saturating_add(1);
    }
    boundaries
}

#[allow(
    clippy::expect_used,
    reason = "folded groups are emitted only after collecting hidden rows"
)]
pub(super) fn project_ledger_rows(
    records: &Vector<Arc<TrajectoryRecord>>,
    matching: &TimelineMatches,
    query_active: bool,
    collapsed_turns: &HashSet<u32>,
    collapsed_assistants: &HashSet<TrajectoryItemId>,
) -> TimelineRows {
    if query_active {
        return TimelineRows::Projected(
            records
                .iter()
                .enumerate()
                .filter_map(|(index, _)| {
                    matching
                        .contains(&index)
                        .then_some(TimelineLedgerRow::Record(index))
                })
                .collect(),
        );
    }
    if collapsed_turns.is_empty() && collapsed_assistants.is_empty() {
        return TimelineRows::All(records.len());
    }

    let mut rows = Vector::new();
    let mut index = 0;
    while index < records.len() {
        let record = &records[index];
        if let Some(turn) = record.turn
            && collapsed_turns.contains(&turn)
        {
            let mut end = index + 1;
            while end < records.len() && records[end].turn == Some(turn) {
                end += 1;
            }
            let content = (index..end)
                .filter(|candidate| records[*candidate].kind != TrajectoryKind::System)
                .collect::<Vec<_>>();
            if content.len() > 1 {
                let first = content[0];
                let hidden = &content[1..];
                let (step_ids, call_count) = turn_summary_aggregate(
                    records,
                    hidden[0],
                    *hidden.last().expect("hidden turn content is nonempty"),
                );
                for candidate in index..end {
                    if candidate == first {
                        rows.push_back(TimelineLedgerRow::Record(candidate));
                        rows.push_back(TimelineLedgerRow::TurnSummary {
                            representative: first,
                            turn,
                            first_hidden: hidden[0],
                            last_hidden: *hidden.last().expect("hidden turn content is nonempty"),
                            step_ids: step_ids.clone(),
                            call_count,
                        });
                    } else if records[candidate].kind == TrajectoryKind::System {
                        rows.push_back(TimelineLedgerRow::Record(candidate));
                    }
                }
                index = end;
                continue;
            }
        }

        rows.push_back(TimelineLedgerRow::Record(index));
        if record.kind == TrajectoryKind::Assistant && collapsed_assistants.contains(&record.id) {
            let first_tool = index + 1;
            let mut end = first_tool;
            while end < records.len() && records[end].kind == TrajectoryKind::Tool {
                end += 1;
            }
            if end > first_tool {
                let (tool_names, tools, tools_truncated) =
                    tool_summary_aggregate(records, first_tool, end - 1);
                rows.push_back(TimelineLedgerRow::CallsSummary {
                    assistant: index,
                    first_tool,
                    last_tool: end - 1,
                    tool_names,
                    tools,
                    tools_truncated,
                });
                index = end;
                continue;
            }
        }
        index += 1;
    }
    TimelineRows::Projected(rows)
}

pub(super) fn turn_summary_aggregate(
    records: &Vector<Arc<TrajectoryRecord>>,
    first_hidden: usize,
    last_hidden: usize,
) -> (ImHashSet<u32>, usize) {
    let mut step_ids = ImHashSet::new();
    let mut call_count = 0_usize;
    for index in first_hidden..=last_hidden {
        let record = &records[index];
        if record.kind == TrajectoryKind::System {
            continue;
        }
        if let Some(step) = record.step {
            step_ids.insert(step);
        }
        call_count = call_count.saturating_add(usize::from(record.kind == TrajectoryKind::Tool));
    }
    (step_ids, call_count)
}

pub(super) fn tool_summary_aggregate(
    records: &Vector<Arc<TrajectoryRecord>>,
    first_tool: usize,
    last_tool: usize,
) -> (ImHashSet<String>, Arc<str>, bool) {
    let mut tool_names = ImHashSet::new();
    let mut tools: Arc<str> = Arc::from("");
    let mut truncated = false;
    for index in first_tool..=last_tool {
        let name = records[index].title.trim();
        if !name.is_empty() && !tool_names.contains(name) {
            tool_names.insert(name.to_owned());
            let (next, next_truncated) = append_tool_summary_preview(&tools, truncated, name);
            tools = next;
            truncated = next_truncated;
        }
    }
    (tool_names, tools, truncated)
}

pub(super) fn append_tool_summary_preview(
    current: &Arc<str>,
    truncated: bool,
    name: &str,
) -> (Arc<str>, bool) {
    if truncated {
        return (Arc::clone(current), true);
    }
    let separator = if current.is_empty() { "" } else { ", " };
    if current
        .len()
        .saturating_add(separator.len())
        .saturating_add(name.len())
        <= TOOL_SUMMARY_PREVIEW_MAX_BYTES
    {
        return (Arc::from(format!("{current}{separator}{name}")), false);
    }

    let marker = if current.is_empty() { "…" } else { ", …" };
    let mut preview = current.to_string();
    let remaining = TOOL_SUMMARY_PREVIEW_MAX_BYTES.saturating_sub(preview.len() + marker.len());
    if current.is_empty() && remaining > 0 {
        let mut end = remaining.min(name.len());
        while end > 0 && !name.is_char_boundary(end) {
            end -= 1;
        }
        preview.push_str(&name[..end]);
    }
    preview.push_str(marker);
    (Arc::from(preview), true)
}

/// Extends the empty-search folded projection without replaying the full session. A turn summary
/// owns exactly the hidden content interval; system rows remain explicit, and call summaries own
/// only the contiguous tool run after an assistant. This keeps append work independent of the
/// already-materialized session length.
#[allow(
    clippy::unreachable,
    reason = "located appended rows cannot be existing turn summaries"
)]
pub(super) fn append_projected_record(
    records: &Vector<Arc<TrajectoryRecord>>,
    index: usize,
    collapsed_turns: &HashSet<u32>,
    collapsed_assistants: &HashSet<TrajectoryItemId>,
    rows: &mut Vector<TimelineLedgerRow>,
    fold_append: &mut FoldAppendState,
) {
    let record = &records[index];
    let continued_turn =
        record.turn.is_some() && index > 0 && records[index - 1].turn == record.turn;
    if !continued_turn {
        *fold_append = record
            .turn
            .filter(|turn| collapsed_turns.contains(turn))
            .map_or_else(FoldAppendState::default, |turn| FoldAppendState {
                tail_turn: Some(turn),
                ..FoldAppendState::default()
            });
    }

    if let Some(turn) = record.turn
        && collapsed_turns.contains(&turn)
        && continued_turn
    {
        debug_assert_eq!(fold_append.tail_turn, Some(turn));
        if record.kind == TrajectoryKind::System {
            rows.push_back(TimelineLedgerRow::Record(index));
            return;
        }

        if let Some(position) = fold_append.turn_summary_row {
            let Some(TimelineLedgerRow::TurnSummary {
                representative,
                first_hidden,
                mut step_ids,
                call_count,
                ..
            }) = rows.get(position).cloned()
            else {
                unreachable!("the located row is a turn summary");
            };
            if let Some(step) = record.step {
                step_ids.insert(step);
            }
            rows.set(
                position,
                TimelineLedgerRow::TurnSummary {
                    representative,
                    turn,
                    first_hidden,
                    last_hidden: index,
                    step_ids,
                    call_count: call_count
                        .saturating_add(usize::from(record.kind == TrajectoryKind::Tool)),
                },
            );
            return;
        }

        if let (Some(first), Some(first_row)) =
            (fold_append.first_content, fold_append.first_content_row)
        {
            let mut step_ids = ImHashSet::new();
            if let Some(step) = record.step {
                step_ids.insert(step);
            }
            let summary_row = first_row + 1;
            rows.insert(
                summary_row,
                TimelineLedgerRow::TurnSummary {
                    representative: first,
                    turn,
                    first_hidden: index,
                    last_hidden: index,
                    step_ids,
                    call_count: usize::from(record.kind == TrajectoryKind::Tool),
                },
            );
            fold_append.turn_summary_row = Some(summary_row);
            return;
        }

        let row = rows.len();
        rows.push_back(TimelineLedgerRow::Record(index));
        fold_append.first_content = Some(index);
        fold_append.first_content_row = Some(row);
        return;
    }

    if record.kind == TrajectoryKind::Tool {
        if let Some(position) = rows.len().checked_sub(1)
            && let Some(TimelineLedgerRow::CallsSummary {
                assistant,
                first_tool,
                last_tool,
                mut tool_names,
                tools,
                tools_truncated,
            }) = rows.get(position).cloned()
            && last_tool + 1 == index
        {
            let name = record.title.trim();
            let (tools, tools_truncated) = if name.is_empty() || tool_names.contains(name) {
                (tools, tools_truncated)
            } else {
                tool_names.insert(name.to_owned());
                append_tool_summary_preview(&tools, tools_truncated, name)
            };
            rows.set(
                position,
                TimelineLedgerRow::CallsSummary {
                    assistant,
                    first_tool,
                    last_tool: index,
                    tool_names,
                    tools,
                    tools_truncated,
                },
            );
            return;
        }
        if index > 0
            && records[index - 1].kind == TrajectoryKind::Assistant
            && collapsed_assistants.contains(&records[index - 1].id)
            && rows.back() == Some(&TimelineLedgerRow::Record(index - 1))
        {
            let (tool_names, tools, tools_truncated) =
                tool_summary_aggregate(records, index, index);
            rows.push_back(TimelineLedgerRow::CallsSummary {
                assistant: index - 1,
                first_tool: index,
                last_tool: index,
                tool_names,
                tools,
                tools_truncated,
            });
            return;
        }
    }
    let row = rows.len();
    rows.push_back(TimelineLedgerRow::Record(index));
    if fold_append.tail_turn == record.turn
        && record.kind != TrajectoryKind::System
        && fold_append.first_content.is_none()
    {
        fold_append.first_content = Some(index);
        fold_append.first_content_row = Some(row);
    }
}

pub(super) fn sorted_record_position(
    values: &Vector<TimelineLedgerRow>,
    needle: usize,
) -> Result<usize, usize> {
    let mut left = 0;
    let mut right = values.len();
    while left < right {
        let middle = left + (right - left) / 2;
        match values[middle].representative_index().cmp(&needle) {
            std::cmp::Ordering::Less => left = middle + 1,
            std::cmp::Ordering::Greater => right = middle,
            std::cmp::Ordering::Equal => return Ok(middle),
        }
    }
    Err(left)
}

pub(super) fn sorted_insert_record(values: &mut Vector<TimelineLedgerRow>, value: usize) {
    if let Err(index) = sorted_record_position(values, value) {
        values.insert(index, TimelineLedgerRow::Record(value));
    }
}

pub(super) fn sorted_remove_record(values: &mut Vector<TimelineLedgerRow>, value: usize) {
    if let Ok(index) = sorted_record_position(values, value) {
        values.remove(index);
    }
}

pub(super) const LEDGER_SUMMARY_ROW_HEIGHT: f32 = 20.0;
