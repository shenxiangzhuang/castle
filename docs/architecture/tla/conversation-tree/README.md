# Conversation tree

Contract: [Conversation tree, message editing and fork](../../conversation-tree.md).
Run `just tla-check conversation-tree` and `just tla-self-test conversation-tree`.

This bounded model has two sessions, three nodes and three committed transactions. Its two
initial nodes are a settled user message and assistant response. `running = {}` abstracts
quiescence, including pending inputs and unresolved tools. One session writer serializes
mutations; `Begin` captures a revision, `Commit` publishes an edit or fork, and `Crash`
discards uncommitted work. `Browse` is a read-only path query, not a desktop tree browser.

The model checks immutable, acyclic parent links; paths containing only ancestors of the
selected head; publication only after commit; atomic, idempotent Fork without moving the
source head; and rejection of branch mutation while running. `self-test` injects an invalid
parent, early publication, duplicate Fork and sibling context leakage, and probes that
edit/Fork are reachable.

| Model state/action | Rust implementation |
| --- | --- |
| `parents`, `heads`, `Path` | `session/tree.rs`, `SessionMachine` context checkpoints |
| `Begin`, `Commit` | `AgentLoop::run_edit`, selection and revision guards; `SessionStore::create_fork` transaction |
| `published`, `Receive`, `Crash` | Commit receipts, canonical document and journal replay |
| `Browse` | Pure path query; relationship navigation does not select a historical head |

Equal node labels abstract copied Fork history: Rust stores normalized path evidence in the
child's first event. The model does not cover SQLite bytes, provider calls, actual tool effects,
compaction, pending input attachment, title allocation, archive UI or project-list changes.
Rust regressions cover these, including a cut lifecycle transaction during Fork import,
`reopened_selected_path_preserves_pending_inputs` and
`fork_completion_keeps_project_identity_after_removal`. Tool and queue interleavings have
separate [session-tools](../session-tools/README.md) and
[input-queue](../input-queue/README.md) models. This bounded check is not a Rust refinement proof.
