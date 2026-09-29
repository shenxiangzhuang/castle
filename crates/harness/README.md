# harness

The execution layer between `agent` and any desktop/CLI client. It owns session
commands, SQLite, model connections, tool execution, approvals, configuration and shutdown.

Build a `SessionSetup`, attach it to `Harness`, then use `SessionHandle::connect` and
`send`/`send_with_id`. `try_send` acknowledges queue admission; its receiver resolves after the
semantic operation. `send` waits for that result. Disconnecting an observer does not stop a run;
`Harness::shutdown` cancels and joins its owners. Internal mutable agents, write permits and
execution tasks are not public APIs.

Public command and subscription queues are bounded. Lagged observers reconnect from an atomic
snapshot. Command reservations and results survive reopening; an unfinished reservation returns
an explicit unknown outcome and never automatically repeats side effects. SQLite schema 3 adds
only the command ledger and migrates schemas 1/2 without rewriting the journal.

No GPUI dependency; no independent crate publication. See the
[core architecture](../../docs/architecture/overview.md) and [session protocol](../../docs/architecture/session.md).
