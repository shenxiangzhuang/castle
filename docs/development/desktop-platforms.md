# Desktop platform acceptance

Contract: [platform presentation](../architecture/desktop.md#desktop-platform-presentation).
Use the same commit, fixture and logical viewport on all three systems. Ubuntu must have
X11 or enabled XWayland plus WebKitGTK; Windows must have WebView2. A Wayland desktop with
XWayland should launch normally without manually removing environment variables.

Build `cargo build -p desktop --locked`. The debug app's existing fixture uses an isolated
store, without API credentials or writes to real sessions:

```sh
CASTLE_DATA_DIR=/tmp/castle-platform-check \
CASTLE_PREVIEW_MARKDOWN="$PWD/crates/desktop/tests/fixtures/platform-ui.md" \
  target/debug/castle-desktop
```

On macOS use `just macos-app-debug`, then the same environment with
`target/Castle.app/Contents/MacOS/castle`. On Windows PowerShell:

```powershell
$env:CASTLE_DATA_DIR = Join-Path $env:TEMP 'castle-platform-check'
$env:CASTLE_PREVIEW_MARKDOWN = Join-Path $PWD 'crates/desktop/tests/fixtures/platform-ui.md'
.\target\debug\castle-desktop.exe
```

Check at 1180 × 720 and the 720 × 620 minimum, in light/dark mode and at 100%, 125%, 150%,
200% scaling (where offered by the OS):

- Latin/CJK text, bold, code and formulas: no missing glyphs, clipped baselines or overlapping
  rows; select/copy mixed text and code without changing its source or whitespace.
- Expanded/collapsed sidebar: title and toggle do not overlap. The shortcut hint matches
  Command-B on macOS and Ctrl-B elsewhere, and the shortcut actually toggles the sidebar.
- Window controls: minimize/restore, maximize/restore, close, fullscreen, dragging blank
  titlebar space and double-clicking it. Repeat with empty chat, archived session, an open
  HTML sidebar, and startup-error view. Check there are no duplicate decoration controls.
- Chinese IME in the composer: compose, select a candidate, edit the middle of text,
  Enter while composing, then ordinary Enter/Shift-Enter. Candidate position must track the
  caret before/after resize or changing scale. Verify Ctrl/Command-A/C/V and keyboard focus.
- HTML: operate the slider/button, scroll out and back, resize, show/copy source, expand to
  the sidebar, then open settings. Check retained state, clipping, wheel handoff and absence
  of native preview creation errors. Also use the more extensive `html-previews.md` fixture.

Record the OS, display backend, GPU/driver, scale, commit, screenshots and observed interactions.
Only an actual native run establishes that platform's result; three-platform compilation and
headless tests alone do not. Identical physical pixels are not required for OS-native controls,
font antialiasing or emoji, but layout/content/interaction must retain the same product contract.
