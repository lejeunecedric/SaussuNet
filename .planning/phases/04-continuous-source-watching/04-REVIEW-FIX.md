---
phase: 4
date: 2026-11-02
findings_fixed: [H1, M2, M1]
findings_skipped: []
---

# Phase 4 Review Fix Report — H1 / M2 / M1

All three assigned findings fixed TDD-style (regression test observed RED
first, then GREEN), one atomic commit each, not pushed.

## H1 — per-file error wedged the source + dropped log.md lines (HIGH)

Commit: `473e64f fix(04): H1 — per-file delta failures no longer wedge the
source or drop log.md lines` (+ `e4f3229 style(04): cargo fmt` follow-up)

Fix (`src/watch/mod.rs`):
- Walk loop (`poll_inner`) and sync loop (`apply_sync_report`): per-file
  errors from `apply_file_delta` are caught inside the loop instead of
  propagated via `?`. The first failure is kept as a sanitized
  (`sanitize_line`, D-11 hygiene — no secrets, no newlines) poll-level
  `CoreError::Dav`; remaining deltas keep applying.
- Accumulated `lines` are flushed to log.md BEFORE the poll-level Err in
  both paths, so applied deltas' `added:`/`changed:` lines are never lost.
  On the sync path the token still does NOT advance (Err propagates through
  `try_sync_poll`'s `?`), so the failed file replays and the sha256 gate
  absorbs the already-applied ones.
- Dotfile edge: an empty stem (`.md`/`.txt`) is now a once-only
  `skipped (empty file stem)` note + marker row (sha256 NULL, same D-08
  discipline) inside `apply_file_delta`, checked before the GET — no longer
  a fatal `sanitize_component("")` error repeating every tick.
- Stub knob added: `fail_get(path)` — persistent 500 on GET of that path
  (the "fails every tick" wedging case).

RED evidence: `failing_file_does_not_wedge_the_walk` failed with
"left: 1, right: 2 — a.md + c.md must enqueue despite b.md";
`sync_failing_file_flushes_log_and_holds_token` and
`dotfile_md_is_skipped_not_fatal` also failed (3 failed / 0 passed).
GREEN evidence: same 3 tests pass; full `watch_webdav` suite 27/27.

Regression tests (`tests/watch_webdav.rs`):
- `failing_file_does_not_wedge_the_walk` — b.md GET 500 forever; a.md and
  c.md still enqueue (2 jobs), log.md contains `added: a.md` AND
  `added: c.md` despite poll Err, last_error names b.md, next poll does not
  re-enqueue the applied files.
- `sync_failing_file_flushes_log_and_holds_token` — sync-path variant: log
  flushed, token held (None), replay absorbed by sha256 gate.
- `dotfile_md_is_skipped_not_fatal` — `.md` skipped once, `ok.md` still
  ingested, poll Ok(1), second poll silent.

## M2 — sync_unsupported latched on transport errors (MEDIUM)

Commit: `5da585f fix(04): M2 — latch sync_unsupported only on HTTP-level
unsupported signals`

Fix (`src/watch/dav.rs` + `src/watch/mod.rs`):
- New `dav::is_sync_unsupported_signal(err)`: true only for the HTTP-level
  non-207 signal (`"expected 207 Multi-Status, got …"` — covers 400/405/501
  and the 200-instead-of-207 case per the plan's non-207 rule).
- `try_sync_poll`: first-attempt failure and stale-token empty-token-retry
  failure latch ONLY on that signal; transport errors (connection refused /
  timeout / request failed) now return `Err` — poll fails, `last_error`
  records it, next tick retries sync. No permanent demotion of a briefly-down
  sync-capable server.

RED evidence: `transport_error_first_poll_does_not_latch_sync_unsupported`
failed with "transport error must NOT latch sync_unsupported (M2)" (1 failed
/ 1 passed — the 400-latch companion test passed pre-fix by design).
GREEN evidence: both pass; `sync_unsupported_falls_back_forever` and
`stale_token_resyncs_not_latches` (existing latch/Pitfall-5 behavior) stay
green.

Regression tests: `transport_error_first_poll_does_not_latch_sync_unsupported`
(closed port → Err, latch unset, last_error set) and
`http_400_first_poll_latches_sync_unsupported` (400 → latch, explicit
counterpart).

## M1 — dot-segment hrefs escaped the watch root (MEDIUM)

Commit: `2963c5c fix(04): M1 — reject dot-segment hrefs at the url_for
choke point`

Fix (`src/watch/dav.rs`): `url_for` — the single choke point every request
URL passes through (propfind_children / sync_collection / get_bytes) — now
rejects any `rel` containing a `.` or `..` path segment with a sanitized
`CoreError::Dav` ("dot-segments are not allowed"). `rel` arrives
percent-DECODED from the multistatus parser, so this catches both literal
`../` and `%2e%2e` traversal; the rejection happens before any request is
sent.

RED evidence: `dot_segment_href_cannot_escape_watch_root` failed with the
stub log literally showing the escape:
`[("POST", "/vault-inbox/", …), ("GET", "/secret.md", None, "")]`;
`url_for_rejects_dot_segments` also failed (2 failed / 0 passed).
GREEN evidence: both pass — no request leaves `/vault-inbox/`, poll records
last_error.

Regression tests: `url_for_rejects_dot_segments` (unit: `..`, `../etc/passwd`,
`a/../../b.md`, `./x.md`, `a/./b.md` all rejected pre-request — stub log
empty) and `dot_segment_href_cannot_escape_watch_root` (hostile sync stub
serving `../../secret.md` against a `/vault-inbox/` root; stub `requests()`
log proves nothing escaped the root).

## Test summary

- Final `cargo test -p saussunet-core`: **266 passed / 0 failed / 1 ignored**
  (llm_live). Workspace `cargo test --workspace`: 277 passed / 0 failed /
  1 ignored. `bash scripts/check.sh`: **check: OK** (fmt, clippy, tests,
  guards, release build).
- 7 new regression tests (3 H1, 2 M2, 2 M1); `watch_webdav` suite 20 → 27.
- Baseline note: the task brief quoted "282 passed" but the measured
  pre-fix baseline at `d8aacf1` is 259 passed + 1 ignored for
  `-p saussunet-core` (verified by test-function census at that commit:
  current count minus exactly my 7 additions — nothing lost; the quoted
  figure appears to have been stale or counted differently).
- Flake seen once under parallel load: `pipeline_url::
  unknown_kind_dead_letters_with_error_retained` (known queue/pipeline_url
  SQLITE_BUSY flake) — passes in isolation and in every subsequent full run.

## Files

- `crates/saussunet-core/src/watch/mod.rs` (H1 loops + dotfile skip, M2 latch gating)
- `crates/saussunet-core/src/watch/dav.rs` (M2 classifier, M1 url_for rejection)
- `crates/saussunet-core/tests/watch_webdav.rs` (fail_get stub knob + 7 regression tests)
