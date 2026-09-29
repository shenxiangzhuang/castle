# Harness connection

Contract: [core architecture](../../overview.md). This bounded safety model covers two command IDs,
a one-event subscriber buffer, two publications, and one cancellable run. It explores reservation,
effect execution, durable result, lost acknowledgment/crash, atomic snapshot/subscription, explicit
lag, observer disconnection and resource join. It does not assert liveness or model SQLite internals,
credentials, model token streams, process killing, post-shutdown channel closure, or execution of the Rust code. Rust tests cover closing both started
and unstarted subscriptions.

| Invariant | Implementation |
| --- | --- |
| AtMostOnce | `SessionStore::reserve_command`; an unfinished reservation returns outcome unknown |
| ConfirmedAfterDurable | `Owner::reply` persists the command result before sending it |
| NoSilentGap | `SessionHandle::connect` and `Owner::publish` share a lock; bounded broadcast reports Lagged |
| ReadyAfterJoin | `Owner::settle` awaits `ActiveAgent::finish`; `Harness::shutdown` awaits the task tracker |
| ObserverIsolation | Dropping a connection never sends Stop; only explicit cancellation shuts a run down |

Mapping: [host.rs](../../../../crates/harness/src/host.rs),
[store.rs](../../../../crates/harness/src/session/store.rs).
Reservation and effects are deliberately separate transactions: after a crash, unfinished commands
remain unknown and cannot be retried automatically. This sacrifices automatic retry progress to avoid
duplicate side effects. A completed result survives reopening. The model assumes durable writes survive
crashes and each unique ID has one semantic body; the Rust store tests also check body conflicts.

`self-test` injects six faults, each required to violate the corresponding invariant. The existing
session/tool and queue models separately check the domain journal and tool authorization rules.
