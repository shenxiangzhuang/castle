use super::*;

pub(super) fn trajectory_details_default_width(main_width: f32) -> f32 {
    (main_width * 0.38).clamp(DETAILS_MIN_WIDTH, DETAILS_DEFAULT_MAX_WIDTH)
}

pub(super) fn sanitized_width(width: f32) -> f32 {
    if width.is_finite() {
        width.max(0.0)
    } else {
        0.0
    }
}

pub(super) fn timeline_bar_gap_px(width_px: f64) -> f64 {
    (width_px.max(0.0) * TIMELINE_BAR_GAP_FRACTION).min(TIMELINE_BAR_GAP_MAX_PX)
}

pub(super) fn clamp_trajectory_details_width(width: f32, split_width: f32) -> f32 {
    let max_width = (sanitized_width(split_width) - LEDGER_MIN_WIDTH)
        .clamp(DETAILS_MIN_WIDTH, DETAILS_MAX_WIDTH);
    sanitized_width(width)
        .clamp(DETAILS_MIN_WIDTH, max_width)
        .round()
}

pub(super) fn resolved_trajectory_details_width(
    mode: TrajectoryMode,
    split_width: f32,
    explicit_width: Option<f32>,
) -> f32 {
    let split_width = sanitized_width(split_width);
    match mode {
        TrajectoryMode::Split => {
            let max_width = (split_width - LEDGER_MIN_WIDTH).max(0.0);
            explicit_width
                .map(|width| sanitized_width(width).clamp(DETAILS_MIN_WIDTH, DETAILS_MAX_WIDTH))
                .unwrap_or_else(|| trajectory_details_default_width(split_width))
                .min(max_width)
        }
        TrajectoryMode::Overlay => explicit_width
            .map(|width| sanitized_width(width).clamp(DETAILS_MIN_WIDTH, DETAILS_MAX_WIDTH))
            .unwrap_or_else(|| {
                (split_width * DETAILS_OVERLAY_FRACTION).min(DETAILS_OVERLAY_MAX_WIDTH)
            })
            .min(split_width * DETAILS_OVERLAY_FRACTION),
        TrajectoryMode::Ledger => 0.0,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct TrajectoryDetailsResizeDrag {
    pub(super) start_x: f32,
    pub(super) start_width: f32,
    pub(super) split_width: f32,
}

#[derive(Debug, Default)]
pub(crate) struct TrajectoryDetailsLayoutState {
    pub(super) explicit_width: Option<f32>,
    pub(super) measured_split_width: Option<(LayoutGeneration, f32)>,
    pub(super) measured_details_width: Option<(LayoutGeneration, f32)>,
    pub(super) drag: Option<TrajectoryDetailsResizeDrag>,
}

impl TrajectoryDetailsLayoutState {
    pub(super) fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    pub(super) fn observe_split_width(&mut self, generation: LayoutGeneration, width: f32) -> bool {
        let changed = Self::observe_width(&mut self.measured_split_width, generation, width);
        if changed {
            self.measured_details_width = None;
        }
        changed
    }

    pub(super) fn observe_details_width(
        &mut self,
        generation: LayoutGeneration,
        width: f32,
    ) -> bool {
        Self::observe_width(&mut self.measured_details_width, generation, width)
    }

    pub(super) fn observe_width(
        measured: &mut Option<(LayoutGeneration, f32)>,
        generation: LayoutGeneration,
        width: f32,
    ) -> bool {
        if !width.is_finite() || width <= 0.0 {
            return false;
        }
        let unchanged = measured.is_some_and(|(measured_generation, measured_width)| {
            measured_generation == generation
                && (measured_width - width).abs() < DETAILS_MEASUREMENT_EPSILON
        });
        if unchanged {
            false
        } else {
            *measured = Some((generation, width));
            true
        }
    }

    pub(super) fn split_width(&self, generation: LayoutGeneration, fallback: f32) -> f32 {
        self.measured_split_width
            .filter(|(measured_generation, _)| *measured_generation == generation)
            .map(|(_, width)| width)
            .unwrap_or_else(|| sanitized_width(fallback))
    }

    pub(super) fn details_width(
        &self,
        mode: TrajectoryMode,
        generation: LayoutGeneration,
        fallback_split_width: f32,
    ) -> f32 {
        resolved_trajectory_details_width(
            mode,
            self.split_width(generation, fallback_split_width),
            self.explicit_width,
        )
    }

    pub(super) fn measured_details_width(&self, generation: LayoutGeneration) -> Option<f32> {
        self.measured_details_width
            .filter(|(measured_generation, _)| *measured_generation == generation)
            .map(|(_, width)| width)
    }

    pub(super) fn begin_drag(&mut self, start_x: f32, start_width: f32, split_width: f32) {
        self.drag = Some(TrajectoryDetailsResizeDrag {
            start_x,
            start_width: sanitized_width(start_width),
            split_width: sanitized_width(split_width),
        });
    }

    pub(super) fn drag_to(&mut self, current_x: f32) -> bool {
        let Some(drag) = self.drag else {
            return false;
        };
        self.set_explicit_width(clamp_trajectory_details_width(
            drag.start_width + drag.start_x - current_x,
            drag.split_width,
        ))
    }

    pub(super) fn end_drag(&mut self) -> bool {
        self.drag.take().is_some()
    }

    pub(super) fn step(&mut self, delta: f32, current_width: f32, split_width: f32) -> bool {
        self.set_explicit_width(clamp_trajectory_details_width(
            current_width + delta,
            split_width,
        ))
    }

    pub(super) fn reset(&mut self) -> bool {
        let changed = self.explicit_width.take().is_some() || self.drag.take().is_some();
        if changed {
            self.measured_details_width = None;
        }
        changed
    }

    pub(super) fn set_explicit_width(&mut self, width: f32) -> bool {
        if self
            .explicit_width
            .is_some_and(|previous| (previous - width).abs() < f32::EPSILON)
        {
            false
        } else {
            self.explicit_width = Some(width);
            self.measured_details_width = None;
            true
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TrajectoryMarkdownSource {
    SystemPrompt,
    Preview,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TrajectoryMarkdownCacheKey {
    pub(super) projection_lineage: u64,
    pub(super) record_id: TrajectoryItemId,
    pub(super) source: TrajectoryMarkdownSource,
}

#[derive(Debug, Default)]
pub(crate) struct TrajectoryDetailsMarkdownCache {
    pub(super) key: Option<TrajectoryMarkdownCacheKey>,
    pub(super) markdown: StreamingMarkdownState,
    pub(super) fallback: SharedString,
    pub(super) selection: Option<crate::rendering::MessageSelection>,
}

impl TrajectoryDetailsMarkdownCache {
    pub(super) fn sync(
        &mut self,
        projection_lineage: u64,
        record_id: &TrajectoryItemId,
        source_kind: TrajectoryMarkdownSource,
        source: &str,
    ) {
        let key = TrajectoryMarkdownCacheKey {
            projection_lineage,
            record_id: record_id.clone(),
            source: source_kind,
        };
        if self.key.as_ref() != Some(&key) {
            self.key = Some(key);
            self.markdown = StreamingMarkdownState::default();
            self.selection = None;
            self.fallback = source.to_owned().into();
        } else if self.fallback.as_ref() != source {
            self.fallback = source.to_owned().into();
        }
        self.markdown.update(source);
    }
}
