# Architecture

The [core architecture](overview.md) is authoritative for layer boundaries, public API,
and tradeoffs. `Status: accepted` means a design is adopted, not necessarily implemented;
target designs identify outstanding migration explicitly. Protocol and presentation documents
retain their narrower contracts. Verification results belong to tests and CI for each commit.

| Document | Responsibility | Formal model |
| --- | --- | --- |
| [Core architecture](overview.md) | SDK / harness / interaction boundaries, public API, tradeoffs, and migration | — |
| [Session](session.md) | Durable facts, transactions, lifecycle, and runtime ownership | [Session tools](tla/session-tools/README.md) |
| [Desktop](desktop.md) | Projection, timing semantics, interaction, and rendering | — |
| [App storage](app-storage.md) | Product configuration and catalog persistence | — |
| [Conversation tree](conversation-tree.md) | Append-only message editing, branch navigation and independent forks | [Conversation tree](tla/conversation-tree/README.md) |

The [TLA+ guide](tla/README.md) indexes executable models and explains how to run
or add them. Each model links back to its architecture contract and records its
scope, assumptions, properties, and implementation mapping.

Update the relevant design document and model together when their contracts change.
Keep operational commands in the [development workflow](../development/workflow.md).
