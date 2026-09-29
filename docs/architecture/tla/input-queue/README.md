# Pending input protocol

This finite model checks the [session input contract](../../session.md#pending-input-control)
with two message identities. It covers submission, promotion of the same identity, cancellation,
attachment at step/turn boundaries, stop/resume, and the gap between a durable commit and its
visible receipt. Message numbers abstract admission order. Stale actions consult current durable
state and have no transition if attachment/cancellation already won.

| Property | Implementation mapping |
| --- | --- |
| `NoLostInput` | `InputSubmitted` owns content until `InputAttached` or `InputCancelled` |
| `AtMostOnce` | `SessionMachine` requires inbox membership before attachment |
| `CancelledNeverAttached` | Cancellation removes inbox membership permanently |
| `StopPausesQueue` | `AgentLoop` observes cancellation before selecting another input; resume is explicit |
| `CommittedVisibility` | `SessionConnection` projects receipts, rather than optimistic queue mutations |

A stop racing with an already selected attachment can lose to that attachment. `Stop` models the
owner observing stop at an operation boundary, not the physical pointer-down event. Similarly,
`Receive` abstracts ordered receipt delivery; model checking does not validate GPUI scheduling.
No provider/tool execution, new-run IDs, crash recovery, SQLite internals, drafts, or rendering
is modeled. Storage atomicity and the existing single-writer protocol are assumptions. Paused
messages survive because stopping does not change durable input state. Actual reopen and replay
are checked in Rust. There is no unconditional liveness claim: a user may keep the queue paused,
a provider may hang, or priority messages may indefinitely precede ordinary messages.

Run `just tla-check input-queue` and `just tla-self-test input-queue`. The self-test injects lost
input, duplicate attachment, cancelled-message attachment, and consumption after stop; each must
produce its named invariant violation. Two reachability probes ensure promotion and paused
pending messages are reachable. These checks complement the independent state-machine oracle in
`pending_input_actions_preserve_identity_and_replay`, the gated HTTP integration test
`pending_messages_can_be_prioritized_cancelled_stopped_and_resumed`, and desktop projection/UI tests.
A passing bounded model does not prove the Rust implementation correct.

Editing uses the existing `Cancel` transition; resending uses `Submit` with a new identity. No new
durable state or model action is needed. Composer handoff, guarding an existing draft, waiting for
withdrawal before resubmission, and stale-action rollback are outside this protocol model and are
checked by `pending_edit_withdraws_before_resubmission_and_preserves_drafts` in the GPUI tests.

Late composer acknowledgements are scoped to the selected runtime. The GPUI tests cover a
successful admission after switching sessions and an edit failure delivered to a background
session; this view-selection behavior does not change the durable protocol modeled here.
