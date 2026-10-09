# HTML publication

Contract: [Desktop HTML previews](../../desktop.md#inline-html-previews).

The retained host waits for fence completion, source preparation and iframe bootstrap readiness
before publishing content. A cold sidebar carries the inline preparation state. The browser parses
the original HTML once and owns DOM-ready/load ordering; document load and font readiness
acknowledge initialization. Only an acknowledgement for the current published generation reveals the
page and releases its buffered initial height. Source rewriting retires the host; late callbacks
cannot dismiss a replacement's loading state.

`CompleteOnly` rejects partial/unprepared publication, `InitializedOnly` rejects premature/stale reveal, and
`AtMostOnce` rejects repeated publication of a generation. Sensitivity faults deliberately bypass
each guard; reachability requires a completed visible document. Mapping: host `publish` and
`previewUpdate`, document `content` and `rendered`, and host message generation checks.

Bounds: an initial generation, one append and one rewrite; callbacks may arrive after rewrite.
The model separates source preparation from fence completion and initialization from publication.
It abstracts browser load/font work as initialization, and does not establish browser implementation
refinement or pixel continuity. JS tests check the host protocol; the macOS CI test
`swift crates/desktop/tests/html-preview-webkit.swift` exercises the production assets in real WebKit,
including module DOM-ready/load listeners, repeated updates and visible syntax errors.
Rust checks cold sidebar preparation, loading mounts and list size hints. Native acceptance checks a realistic streamed
fixture, completion, interaction, and the right Chat scrollbar. Asynchronous work started by user
scripts after initial rendering remains live; it is not included in initialization completion.
