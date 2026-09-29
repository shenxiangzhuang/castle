# Castle desktop

`desktop` is the native GPUI interface for `harness`. It provides project-scoped
session history, streaming chat, approvals, trajectory inspection, responsive layout, and native
macOS window behavior.

## Run on macOS

Launch the bundled application:

```sh
just macos-run
```

Use `just macos-run-debug` for a debug build. These recipes create and sign
`target/Castle.app` before launching it. A bare `cargo run -p desktop` starts an
unbundled executable and does not reproduce all AppKit behavior, especially fullscreen titlebar
reveal. Configure at least one provider in **Settings → Models** before starting a session.

Desktop preferences, provider catalogs, credentials, and the project registry are stored in the
app-level SQLite WAL database `~/.castle/app.sqlite3`. The Models tab follows the DSH provider-card
flow: configured providers stay editable, while Add provider lets you choose OpenAI or DeepSeek
before entering its API key and editing its model catalog. The composer only lists models from
providers with configured credentials. API keys are never rendered back into the form, and the app
database and its WAL sidecars use user-only permissions on Unix. Global model and permission choices
are defaults for new sessions.

Quit Kcastle before the first Castle launch. If `~/.castle` does not exist, the old `~/.kcastle`
directory moves there automatically; existing directories are never merged or overwritten.
See the [storage contract](../../docs/architecture/app-storage.md#layout).

Each project persists session metadata and append-only transactions in its own SQLite WAL database
at `~/.castle/projects/<project-id>/sessions/sessions.sqlite3`. The built-in Default project follows
the same layout at `~/.castle/projects/default/sessions/sessions.sqlite3`; there is no separate
top-level Default session store. JSONL is export-only.

Development and isolated acceptance runs can set `CASTLE_DATA_DIR` to put the app database and
every project session store under a separate data root. An absolute path is
recommended; a relative path is resolved from the process working directory. The variable must not
be empty. Normal launches leave it unset and continue to use `~/.castle`.

In the composer, press `Enter` to send and `Shift+Enter` to insert a newline.
Assistant Markdown renders inline formulas delimited by `$...$` or `\(...\)` and display formulas
delimited by `$$...$$` or `\[...\]`.

Drag to select text in user messages, assistant Markdown, expanded reasoning, and trajectory
Markdown details. Press `Cmd+C` on macOS (`Ctrl+C` elsewhere) to copy, or `Escape` to clear the
selection. Selection can span chat messages. Copies preserve code whitespace, use tabs between
table cells, and export selected formulas as LaTeX. Visual padding around inline code is omitted.
Selections retain their text across scrolling, resizing, and append-only streaming; replacing
selected content or switching sessions clears stale selections. Edge-drag auto-scroll is not
currently connected; use the scroll wheel to navigate longer selections.

## UI automation contract

Core controls expose stable accessibility identifiers through GPUI/AccessKit. On macOS these are
available as `AXIdentifier` values and are preferred over visible text or accessibility-tree
positions in automation:

| Flow | Identifier |
| --- | --- |
| New session | `castle.session.new` |
| Session search input | `castle.session.search.input` |
| Chat / Trajectory tabs | `castle.conversation.chat`, `castle.conversation.trajectory` |
| Composer input group | `castle.composer.input` |
| Permission / model controls | `castle.composer.permission`, `castle.composer.model` |
| Send / stop | `castle.composer.send`, `castle.composer.stop` |
| Tool approval | `castle.approval.allow`, `castle.approval.deny` |
| Trajectory search input | `castle.trajectory.search.input` |
| Settings / active dialog | `castle.settings.open`, `castle.dialog` |

The multiline text input is the `MultilineTextInput` descendant of `castle.composer.input` and is
labelled `Message the agent`. Workspace and session identifiers are derived from their durable
project and session IDs instead of list positions. Treat identifiers in `rendering/automation.rs` as a
compatibility contract: layout and visible copy may change without renaming them.

The desktop trajectory surface is projected directly from session events rather than chat rows. It
has fixed Input, Model, and Tools swimlanes. Duration switches equal-width operations to recorded
durations with idle time compressed, matching the DSH desktop surface; complete wall-clock timing
remains an internal projection. Dragging across the overview focuses the interval, dims operations
outside it, and scrolls the ledger to the focused records. Scroll to zoom around the pointer,
right-drag to pan, double-click or press Escape to clear the focus. Tool
bars distinguish the full call lifecycle from nested execution time; assistant bars distinguish
TTFT from generation. Hovering a bar restores its full color, outlines it, and reveals the DSH
timing tooltip after a short delay; hovering empty lane space shows a vertical time cursor. The
Timing tab uses the same recorded timestamps and can toggle Started
between local and Unix time. The composer footer summarizes whole-session turn/step counts, LLM and
tool wall time, average TTFT, decode throughput, cache hit rate, and token usage. Session catalog
entries that fail validation are ignored by the desktop UI. Request boundary markers open an
independent Request inspector whose Options, Usage, and Timing tabs use only canonical recorded
data; System Prompt, Tools, and Tool Schema views share the same immutable request snapshot.
The Chat / Trajectory choice, trajectory search, folds, selection, details history, focus, viewport,
tail-follow state, and scroll position are retained in memory independently for each session;
new sessions and app restarts default to Chat. Duration mode is a persisted desktop
preference shared by sessions. Timeline statistics update
incrementally, while geometry is cached by session, event revision, and mode and then cheaply
reprojected for the current viewport; Duration coordinates use a merged busy-time index rather
than rescanning every interval for every item.

DMG, AppImage, and Setup EXE builds check `updates.castle.mathewshen.me` hourly and download a
newer release in the background. After the complete package passes its checksum, a compact update
button appears beside Settings. Restart is blocked while any session is active; otherwise the
updater waits for the old process to exit, installs the downloaded package, and launches the new
version. DEB, source, and unbundled development builds do not auto-update.

## Architecture

The [core architecture](../../docs/architecture/overview.md) defines the dependency
`Desktop / CLI -> Harness -> SDK`. Desktop sends commands and projects snapshots/events;
`SessionConnection` owns UI projections and pending acknowledgments, while the harness owns execution.
Projects group sessions, not scheduling: sessions run independently of the selected UI view.

[Desktop architecture](../../docs/architecture/desktop.md) owns projection, interaction and
rendering contracts; [Session](../../docs/architecture/session.md) owns durable semantics.
The [module map](../../docs/architecture/overview.md#层内模块) defines source ownership.
`architecture_tests.rs` enforces crate dependencies, pure projection/layout, shared rendering,
draw-phase and render-time filesystem boundaries.

## Verification

Run the complete local gate from the workspace root:

```sh
just qa
```

For native UI changes, also exercise the signed release bundle manually:

- Collapse and expand the sidebar and confirm the titlebar controls do not move vertically.
- Enter fullscreen, move the pointer to the top edge, and confirm all traffic lights reappear;
  exit fullscreen and confirm their windowed position is restored.
- Stream a response, scroll away from the tail, and confirm new output does not seize the scroll
  position; then use Back to bottom.
- Start at least two sessions in one project and two in different projects; switch among them while
  they stream, approve, queue, stop, and finish independently.
- Confirm a background session uses a spinning ring while running, changes to a blue unread dot
  after it finishes, and clears the dot when selected; the relative time label must remain visible.
- Confirm unsupported or damaged sessions are silently omitted while valid catalog entries remain
  visible.
- Hover a session row and confirm rename appears before archive without selecting the row; restore
  or permanently delete archived sessions from the matching project in Settings.
- Resize through compact, regular, split, and overlay layouts and confirm the composer never
  covers chat or trajectory content.
