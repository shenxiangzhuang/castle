//! Pure run decisions. IDs and observation time come from the host.
use crate::{
    EasyInputMessage, EventDraft, EventTime, InputId, InputItem, InputOrigin, PlannedBatch, RunId,
    RunOutcome, SessionEvent, SessionMachine, SessionMachineError, StepId, StepOutcome,
    TurnEndReason, TurnId, TxId,
};

pub enum AgentInput {
    Start {
        input: Option<(InputId, String)>,
        selected_head: Option<Option<u64>>,
        run: RunId,
        turn: TurnId,
        step: StepId,
    },
    StepCompleted {
        had_tools: bool,
        next_turn: TurnId,
        next_step: StepId,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub enum AgentEffect {
    RequestModel { step: StepId },
    Finished,
}

pub struct Transition {
    batch: PlannedBatch,
    effect: AgentEffect,
}
impl Transition {
    pub fn events(&self) -> &[crate::RecordedEvent] {
        self.batch.events()
    }
    pub fn into_parts(self) -> (PlannedBatch, AgentEffect) {
        (self.batch, self.effect)
    }
}

impl SessionMachine {
    /// Closes all affected requests, tools, steps and turns in one candidate transaction.
    pub fn plan_termination(
        &self,
        reason: TurnEndReason,
        message: String,
        tx_id: TxId,
        time: EventTime,
    ) -> Result<Option<PlannedBatch>, SessionMachineError> {
        let (outcome, step_outcome) = match reason {
            TurnEndReason::Aborted => (RunOutcome::Aborted, StepOutcome::Aborted),
            TurnEndReason::Failed | TurnEndReason::MaxTurns => {
                (RunOutcome::Failed, StepOutcome::Failed)
            }
            TurnEndReason::Completed | TurnEndReason::ToolConcluded => {
                return Err(SessionMachineError::Invalid(
                    "successful completion must use the normal step transition".into(),
                ));
            }
        };
        let Some(recovery) = self.plan_recovery(tx_id.clone(), time)? else {
            return Ok(None);
        };
        let drafts = recovery
            .into_events()
            .into_iter()
            .map(|record| {
                let event = match record.event {
                    SessionEvent::ModelRequestFailed { request_id, .. } => {
                        SessionEvent::ModelRequestFailed {
                            request_id,
                            error: message.clone(),
                        }
                    }
                    SessionEvent::CompactionFinished {
                        compaction_id,
                        summary,
                        response,
                        ..
                    } => SessionEvent::CompactionFinished {
                        compaction_id,
                        outcome: step_outcome,
                        summary,
                        response,
                    },
                    SessionEvent::StepTerminated { step_id, .. } => SessionEvent::StepTerminated {
                        step_id,
                        outcome: step_outcome,
                        error: Some(message.clone()),
                    },
                    SessionEvent::TurnTerminated { turn_id, .. } => {
                        SessionEvent::TurnTerminated { turn_id, reason }
                    }
                    SessionEvent::RunTerminated { run_id, .. } => SessionEvent::RunTerminated {
                        run_id,
                        outcome,
                        error: Some(message.clone()),
                    },
                    event => event,
                };
                EventDraft {
                    tx_id: tx_id.clone(),
                    time: record.time,
                    event,
                }
            })
            .collect();
        self.plan_batch(drafts).map(Some)
    }

    /// Produces a state-bound candidate; the host commits it before applying or executing it.
    pub fn transition(
        &self,
        input: AgentInput,
        tx_id: TxId,
        time: EventTime,
    ) -> Result<Transition, SessionMachineError> {
        let pending = |origin| {
            self.pending_inputs()
                .into_iter()
                .find(|input| input.origin == origin)
        };
        let mut events = Vec::new();
        let effect = match input {
            AgentInput::Start {
                input,
                selected_head,
                run,
                turn,
                step,
            } => {
                if let Some(head) = selected_head {
                    events.push(SessionEvent::ConversationHeadSelected { head });
                }
                let (input_id, text) = if let Some((input_id, text)) = input {
                    if text.trim().is_empty() {
                        return Err(SessionMachineError::Invalid(
                            "agent input must not be empty".into(),
                        ));
                    }
                    events.push(SessionEvent::InputSubmitted {
                        input_id: input_id.clone(),
                        input: text.clone(),
                        origin: InputOrigin::Initial,
                    });
                    (input_id, text)
                } else {
                    let input = pending(InputOrigin::Steer)
                        .or_else(|| pending(InputOrigin::Queue))
                        .ok_or_else(|| {
                            SessionMachineError::Invalid("no pending messages".into())
                        })?;
                    (input.input_id, input.input)
                };
                events.extend([
                    SessionEvent::RunStarted {
                        run_id: run.clone(),
                    },
                    SessionEvent::TurnStarted {
                        run_id: run,
                        turn_id: turn.clone(),
                    },
                    SessionEvent::StepStarted {
                        turn_id: turn,
                        step_id: step.clone(),
                    },
                    SessionEvent::InputAttached {
                        input_id,
                        step_id: step.clone(),
                        items: user_items(text),
                    },
                ]);
                AgentEffect::RequestModel { step }
            }
            AgentInput::StepCompleted {
                had_tools,
                next_turn,
                next_step,
            } => {
                let run = self
                    .active_run()
                    .cloned()
                    .ok_or_else(|| SessionMachineError::Invalid("no active run".into()))?;
                let turn = self
                    .active_turn()
                    .cloned()
                    .ok_or_else(|| SessionMachineError::Invalid("no active turn".into()))?;
                let step = self
                    .active_step()
                    .cloned()
                    .ok_or_else(|| SessionMachineError::Invalid("no active step".into()))?;
                events.push(SessionEvent::StepTerminated {
                    step_id: step,
                    outcome: StepOutcome::Completed,
                    error: None,
                });
                let steer = pending(InputOrigin::Steer);
                let queued = if steer.is_none() && !had_tools {
                    pending(InputOrigin::Queue)
                } else {
                    None
                };
                if steer.is_some() || had_tools || queued.is_some() {
                    let turn = if queued.is_some() {
                        events.push(SessionEvent::TurnTerminated {
                            turn_id: turn,
                            reason: TurnEndReason::Completed,
                        });
                        events.push(SessionEvent::TurnStarted {
                            run_id: run,
                            turn_id: next_turn.clone(),
                        });
                        next_turn
                    } else {
                        turn
                    };
                    events.push(SessionEvent::StepStarted {
                        turn_id: turn,
                        step_id: next_step.clone(),
                    });
                    if let Some(input) = steer.or(queued) {
                        events.push(SessionEvent::InputAttached {
                            input_id: input.input_id,
                            step_id: next_step.clone(),
                            items: user_items(input.input),
                        });
                    }
                    AgentEffect::RequestModel { step: next_step }
                } else {
                    events.push(SessionEvent::TurnTerminated {
                        turn_id: turn,
                        reason: TurnEndReason::Completed,
                    });
                    events.push(SessionEvent::RunTerminated {
                        run_id: run,
                        outcome: RunOutcome::Completed,
                        error: None,
                    });
                    AgentEffect::Finished
                }
            }
        };
        let batch = self.plan_batch(
            events
                .into_iter()
                .map(|event| EventDraft {
                    tx_id: tx_id.clone(),
                    time: time.clone(),
                    event,
                })
                .collect(),
        )?;
        Ok(Transition { batch, effect })
    }
}

fn user_items(message: String) -> Vec<InputItem> {
    vec![InputItem::from(EasyInputMessage::from(message))]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn start_is_pure_and_effect_requires_the_original_state_candidate() {
        let mut state = SessionMachine::default();
        let time = EventTime {
            wall_time_ms: 1,
            clock_id: "test".into(),
            monotonic_ns: 1,
        };
        let transition = state
            .transition(
                AgentInput::Start {
                    input: Some((InputId::from_raw("input"), "hello".into())),
                    selected_head: None,
                    run: RunId::from_raw("run"),
                    turn: TurnId::from_raw("turn"),
                    step: StepId::from_raw("step"),
                },
                TxId::from_raw("tx"),
                time,
            )
            .unwrap();
        assert!(state.active_run().is_none());
        let (candidate, effect) = transition.into_parts();
        assert_eq!(
            effect,
            AgentEffect::RequestModel {
                step: StepId::from_raw("step")
            }
        );
        assert_eq!(candidate.events().len(), 5);
        state.apply_batch(candidate).unwrap();
        assert_eq!(state.active_run(), Some(&RunId::from_raw("run")));
        assert_eq!(state.pending_inputs().len(), 0);
    }
}
