---
phase: 4
plan: 07
one_liner: "opportunistic RFC 6578 sync-collection: POST-as-REPORT client, token round-trip via sync_state, one empty-token stale retry, sync_unsupported latch to the PROPFIND walk forever, sync 404-items as free removals"
requirements_completed: [SRCC-02]
---

# Plan 04-07 Summary — opportunistic RFC 6578 sync-collection (pageTokens)

## What was built

- `crates/saussunet-core/src/watch/dav.rs` — `SyncReport { entries, token }` +
  `DavClient::sync_collection(rel, token)`:
  - POST-as-REPORT (RFC 6578 §3.2): `Depth: 0`, body verbatim per plan
    (`sync-token` empty-or-stored, `sync-level: infinity`, prop = getetag +
    getcontentlength). `sync-level` infinity is the REPORT's level at the
    root — one token per source (D-05); PROPFIND still sends only `Depth: 1`
    (SRCC-01; acceptance grep proves it).
  - 207 required; any other status is an Err so the CALLER decides
    stale-retry vs latch. Same 8 MiB `read_capped` + roxmltree parse and the
    SAME sanitized `display_target` error format as `propfind_children`
    (no forked error format — D-02).
  - Token treated as an OPAQUE string: XML-escaped (`&<>"`) into the request
    body, size-capped at `MAX_SYNC_TOKEN_CHARS = 4096` chars BEFORE
    persisting, stored only parameter-bound via `set_sync_token`, never
    interpolated into a URL or command (threat model).
- `crates/saussunet-core/src/watch/mod.rs` — `poll_once` sync branch
  (`try_sync_poll`), inserted before the BFS walk:
  - State machine per plan: read `get_sync_state`; if not latched, attempt
    `sync_collection("", stored-or-empty)`. Err WITH stored token ⇒ retry
    ONCE with empty token (Pitfall 5). Err on the empty-token attempt (or on
    the first attempt when no token was stored) ⇒ `set_sync_unsupported(true)`
    and fall through to the walk THIS poll and forever (D-05). Err AFTER a
    successful report (download/submit failure) propagates WITHOUT latching
    and WITHOUT advancing the token — next tick retries the same delta.
  - `apply_sync_report`: 200-items → `diff_walk` against watch_state for
    Added/Changed/Skipped classification (allowlist + once-only skip marker
    semantics identical to the walk), 404-items → `- removed:` log line +
    `delete_watch_row`. diff_walk's `Removed` arm is explicitly ignored with
    a comment: a sync delta is NOT a complete universe, so walk-style
    diff-based removals can never fire on the sync path (removal-integrity
    threat model). ONE `append_entry` per poll, only when lines exist.
  - Token persisted via `set_sync_token` ONLY after the delta was fully
    applied (at-least-once: crash between apply and token replays the delta,
    absorbed by the sha256 gate — same order as Pitfall 2). Truncation
    (RFC 6578 §3.6) falls out of the design: whatever token comes back is
    persisted and the next poll continues — no in-poll report loop.
  - Shared `apply_file_delta` helper (GET → sha256 gate → submit → row upsert
    after the durable job) now serves BOTH the walk and the sync paths — the
    04-05 crash-safe order lives in exactly one place. A small `DeltaCtx`
    struct bundles the per-poll context (clippy `too_many_arguments` fix,
    keeps both call sites honest).
  - `spawn_watcher` (04-06) untouched — mode awareness lives in sync_state,
    not the loop, exactly as 04-06 anticipated.
- `crates/saussunet-core/tests/watch_webdav.rs` — stub upgrade:
  `set_sync_mode(SyncMode::Unsupported | Enabled { script })` with
  `SyncReply { items: Vec<(path, ItemStatus)>, token }` served in order (last
  repeats), bodies in the SYNC207 fixture shape (per-item `<d:status>`
  200/404 + trailing `<d:sync-token>`, copied verbatim from the
  watch/multistatus.rs tests); 200-items take etag/size from the tree so the
  poller's GET succeeds. `set_sync_reject_token(s)` answers 400 to any POST
  whose body contains `s` (stale-token simulation). Default stays
  `Unsupported` (POST ⇒ 400) so every pre-existing test keeps the walk path.

## Tests added (3; all RED observed before GREEN)

- `sync_token_persisted_and_reused` (T8a): poll 1 POSTs with
  `<d:sync-token></d:sync-token>` + `<d:sync-level>infinity</d:sync-level>`,
  Depth: 0; token `http://stub/sync/1` persists in `sync_state.token`; poll
  2's recorded POST body carries that token; 404-item ⇒ `removed: gone.md`
  log + row pruned + token advances to sync/2; zero PROPFINDs ever.
  **RED**: `token left: None right: Some("http://stub/sync/1")`.
- `sync_unsupported_falls_back_forever` (T8b): POST 400 ⇒ latch after
  poll 1 (walk still ingests in the SAME poll), exactly ONE POST ever across
  three polls, later file still ingested via the walk.
  **RED**: `assertion failed: sync.sync_unsupported, "D-05 latch set after 400"`.
- `stale_token_resyncs_not_latches` (Pitfall 5): seeded expired token ⇒
  first POST (body contains it) 400s, exactly one empty-token retry
  succeeds; `sync_unsupported` stays false, `last_error` None, token
  advanced, zero PROPFINDs. **RED**: `token left: Some("stale-token")
  right: Some("http://stub/sync/1")`.

## Verification

- `cargo test -p saussunet-core` → **259 passed, 0 failed** (baseline 256 + 3).
- `cargo clippy -p saussunet-core --all-targets -- -D warnings` clean.
- `bash scripts/check.sh` → check: OK.
- Acceptance greps: `sync-level` shows `infinity` in dav.rs; `Depth` shows
  PROPFIND `"1"` (line 116) and sync POST `"0"` (line 154) only.
- T1–T11 regression: all prior watch_webdav tests green unchanged (default
  stub mode keeps every legacy test on the walk path; the one extra 400 POST
  per source on first poll is invisible to their assertions).

## Deviations

- Task-boundary order: the stub SyncMode + all three tests were authored
  first for the RED run (TDD), then committed as Task 2 AFTER the Task 1
  implementation commit so each commit is bisectably green (same approach as
  04-05's recorded boundary blur).
- Plan sketched `apply_file_delta(...)` as a plain multi-arg helper; clippy
  `-D warnings` rejected 10 args, so the shared context became a `DeltaCtx`
  struct — same sharing intent, cleaner call sites.
- Stale-token retry triggers on ANY error with a stored token (incl. network
  errors), matching the plan's literal "failure WITH non-empty token ⇒
  retry ONCE"; the retry is bounded to one per poll either way.
- Sync 404-items are logged/pruned unconditionally (even when no state row
  existed — `delete_watch_row` is a no-op then), per the plan's fixture
  expectation ("gone.md" was never added yet must log + be absent).

## Decisions

- Token advances only AFTER the delta is fully applied (at-least-once delta
  application; a persistently failing item retries each tick instead of
  being silently skipped by an advanced token).
- A sync report that succeeds but whose application fails does NOT count as
  "failure of the sync attempt" — no latch, no token change; the interval
  loop's next tick retries (Err ⇒ last_error recorded as usual).
- Empty-string stored tokens are treated as no-token (`filter(|t| !t.is_empty())`)
  so a cleared token column re-runs the initial sync instead of sending a
  meaningless empty-but-present report distinction.
- Stub `SyncMode::Enabled` keeps the plan's exact shape; the stale-token knob
  is a separate `set_sync_reject_token` rather than a plan-unlisted
  `Enabled` field.

## Lessons

- The parser already carried per-item `status` (04-03) and the walk already
  filtered `status == 200` — the sync wiring was pure orchestration; the
  unused 404 signal was the only dead wire to pick up.
- `diff_walk` can be reused on a sync delta for Added/Changed/Skipped
  classification, but its `Removed` arm MUST be explicitly neutralized with a
  comment — a reviewer's one-line check for "delta ≠ universe" safety.
- Stub POST scripting needs a request counter (Arc<AtomicUsize>) shared
  across connections, not a per-connection index — reqwest opens fresh
  connections per request.
- clippy `too_many_arguments` on a shared helper is a design signal: the
  walk and sync paths wanted a context object, and both call sites got
  shorter as a result.
