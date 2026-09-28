# Session v2

Status: accepted

## Decision

Session v2 uses a project-local SQLite WAL database as the only durable source of truth.
JSONL is an export format, not a runtime database. A single session engine owns the session
machine, serializes commands, commits complete domain transactions, evolves the in-memory machine
only after commit, publishes the committed transaction, and only then starts external effects.

The desktop projects committed transactions into its canonical document; see
[Desktop architecture](desktop.md) for projection, presentation, and interaction contracts.

Session v1 JSONL files are intentionally unsupported and are ignored by the catalog. There is no
dual-write or compatibility fallback.

## Invariants

1. Only a committed transaction is a durable fact.
2. One logical state transition is either wholly committed or wholly absent.
3. One actor owns each active session machine.
4. `SessionMachine::plan_batch` is pure and validates a complete candidate transition.
5. The live machine evolves only from the store's commit receipt; persistence errors never trigger
   an in-memory rollback.
6. A UI subscriber failure cannot change durable state or fail an already committed command.
7. External model and tool effects start only after a durable intent is committed.
8. Commands and effects carry stable typed correlation IDs; stale results cannot mutate a newer
   attempt.
9. A non-idempotent tool interrupted after dispatch is `UnknownSideEffects` and is never retried
   automatically.

## Transaction protocol

Every append carries `session_id`, `tx_id`, `expected_revision`, and an ordered event batch. The
store performs an idempotency lookup, compares the expected revision, writes the transaction and
all events, and advances the session revision in one SQLite transaction. A repeated `tx_id` with
the same digest returns the original receipt; a different digest is corruption.

If the connection loses the commit result, the store resolves the outcome by querying `tx_id`
before accepting another append. Event observation time is distinct from transaction commit time.
Durations use monotonic time only within one clock/boot identity; wall time is used for actual-time
placement.

Journal digest verification reserializes recorded events, including arbitrary JSON in tool schemas.
All builds explicitly enable `serde_json/preserve_order` so replay retains the persisted object-key
order. This reads both sorted and insertion-ordered historical objects without changing their
digests; the storage contract must not depend on desktop dependencies enabling that feature.

## Atomic domain boundaries

- `InputSubmitted` durably owns an input. `InputAttached` simultaneously removes it from the inbox,
  places the user surface item, and binds it to one run/turn/step. There is no `InputConsumed`.
  `InputPrioritized` changes the pending input origin to steering without changing its identity;
  `InputCancelled` removes inbox membership while retaining the journal record.
- Assistant completion and its finalized tool declarations are one transition.
- Tool authorization and dispatch intent are committed before any runner task exists. The runner
  samples execution start immediately before invoking the tool and reports that observation back
  to the single owner; the owner persists start, finish, and result attachment as distinct facts.
  A crash after durable dispatch but before a durable finish is conservatively recovered as
  `UnknownSideEffects`, while an already durable success or error outcome is preserved.
- Normal completion, cancellation, failure, and crash recovery close all affected requests, tools,
  compactions, steps, turns, and runs in one terminal transaction.
- A request is built from the full canonical request snapshot that was committed immediately before
  dispatch, never from a second mutable configuration path.
- `Model` contains only connection data and static capabilities. The active model selection and
  reasoning effort belong to `SessionConfig`; desktop settings supply defaults only when a session
  is created.
- Ordinary requests and compactions resolve those two inputs once, persist the actual model,
  reasoning effort, and output limit before dispatch, and build the provider request from the same
  resolved values.
- Compaction commits a summary boundary and retains recent input batches in the canonical replayed
  surface; it never rewrites prior journal transactions.

## Runtime ownership and control

- An idle `Agent` owns one replayable `SessionMachine` and one concrete transactional
  `SessionStore`; there is no second validator or mutable persistence mirror.
- `Agent::start(self, input)` transfers the agent to one background task. `ActiveAgent::finish()`
  returns ownership after the operation settles.
- `RunControl` sends steering, queued input, approvals, and cancellation without shared mutable
  agent state.
- Steering runs after the current response and its tools. Queued input runs after the agent would
  otherwise settle.

## Verification gates

- The bounded [TLA+ session/tool model](tla/session-tools/README.md) checks dispatch intent,
  commit receipt gaps, ordered attachment, and terminal recovery under cancellation and crashes.
  Its storage assumptions and conditional progress guarantee are documented separately from the
  implementation tests; it does not prove correctness of the Rust implementation.
- Store fault injection proves multi-event transactions are all-or-none and `tx_id` is idempotent.
- A real subprocess hard-kill test proves an interrupted SQLite transaction rolls back and releases
  its writer lock. Recovery-prefix tests cover request, tool, compaction, and terminal lifecycle
  boundaries, including parallel tools whose durable completion and ordered attachment differ.
- Property tests prove live apply equals replay and that invalid states are unreachable.
- A 100k-event storage benchmark checks replay and storage footprint.
- Desktop projection and presentation checks are documented in
  [Desktop architecture](desktop.md#verification-gates).

## Pending input control

Desktop submission while running always queues. A queue entry can be prioritized or cancelled
by its durable `InputId`. The owner serializes these commands with attachment: already attached
or cancelled inputs reject stale actions without failing the active run. Within each priority,
admission order is preserved; steering takes precedence at a response/tool boundary, while queued
input starts a new turn only when the current turn would settle. Neither interrupts live tools.

Submission acknowledgements follow committed `InputSubmitted` events. Stop is observed before
selecting another pending input; an already selected/committing attachment may win a simultaneous
stop. Cancellation never deletes remaining pending messages and does not automatically restart.
`Agent::resume_pending` explicitly starts with the oldest prioritized message, otherwise the oldest
queued message, reusing its identity. Sending a new message while idle is also an explicit new run;
remaining messages are then drained after that message. Reopening only reconstructs pending state.
An idle cancellation acquires the same writer, reloads/recovers the session, and returns all
committed receipts to the host even if the requested cancellation loses to earlier consumption.

The [input queue model](tla/input-queue/README.md) checks bounded protocol interleavings alongside
Rust property and gated HTTP tests. The session/tool model retains its narrower tool boundary.

## Conversation branches and fork

The [conversation tree contract](conversation-tree.md) defines append-only head selection,
atomic editing and independent Fork seeds. `SessionMachine` validates path contexts and
requires quiescence, including an empty pending queue. A Fork seed is the child's first event;
its normalized path evidence is validated before child metadata, journal and origin index
commit together. The source head remains unchanged. SQLite schema 2, event format 4,
search extractor 2 and machine semantics 2 fence incompatible readers. The bounded
[conversation-tree model](tla/conversation-tree/README.md) checks this protocol.
