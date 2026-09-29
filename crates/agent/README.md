# agent

The environment-free SDK in `Desktop / CLI → Harness → SDK`.

`SessionMachine` owns replayable domain state. `transition(AgentInput, TxId, EventTime)` plans
run admission and step/turn progression; `plan_batch` validates observed event batches and
`plan_recovery` / `plan_termination` close interrupted, cancelled or failed operations. Planning never mutates effective state. A host
commits the candidate, applies it, then executes its `AgentEffect`. Context and compaction rules
also live here. The crate uses only Responses data types, not its HTTP client.

No SQLite, Tokio, credentials, processes, or GPUI. No independent crate publication.
See the [core architecture](../../docs/architecture/overview.md) for contracts and tradeoffs.
