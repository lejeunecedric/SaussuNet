---
phase: 4
plan: 06
one_liner: "spawn_watcher interval loop (immediate first poll, WARN-only failures, deterministic jitter) + boot() wiring with ctrl_c teardown + file-backed-SQLite restart-resume proof (T5/T6)"
requirements_completed: [SRCC-01, SRCC-02]
---

# Plan 04-06 Summary — daemon lifecycle: interval loop, boot wiring, restart-resume proof

## What was built

- `crates/saussunet-core/src/watch/mod.rs` — `spawn_watcher(store, vault, src,
  max_fetch_bytes) -> JoinHandle<()>`, exactly per plan Context:
  - **First-poll jitter (RESEARCH open Q5)**: `DefaultHasher(source_id) % 5000 ms`
    capped at `interval / 10` — deterministic, no rand dep; 10 ms-interval
    tests start in <1 ms.
  - **D-10**: `tokio::time::interval(src.interval)` — first tick fires
    immediately (poll on startup), one task per source, missed ticks skipped
    (no backlog).
  - **D-11**: failure ⇒ `tracing::warn!(source = %src.source_id, "watch poll
    failed: {e}")` only — poll_once errors are pre-sanitized (04-05 proves no
    creds in `last_error`; same strings reach this line). Success ⇒ DEBUG
    with `enqueued` count, no per-file logging. The interval IS the retry
    loop — no in-poller backoff.
  - Silent-watcher posture documented in the doc comment (same as worker pool;
    Phase 5 UI surfaces last-poll status).
- `crates/saussunet/src/main.rs` `boot()` — D-12 wiring: worker block
  restructured to `store.clone()`/`vault.clone()` (provider still moves into
  `spawn_workers`; WR-03 gating semantics unchanged); watcher block spawns one
  `spawn_watcher` task per `rt.watch` entry after the worker pool, printing
  `watch ready: N source(s)` only when N > 0; `watch_handles` aborted in the
  existing ctrl_c teardown loop. Zero `[[watch]]` ⇒ zero tasks ⇒ byte-stable
  boot output (verified: release binary in an empty dir prints exactly the
  pre-plan three lines).
- `crates/saussunet-core/tests/watch_webdav.rs` — two new tests (below).
  `ResolvedWatch` already derived `Clone` (04-01) — no change needed.

## Tests added (2)

- `interval_loop_polls_on_spawn_and_repeats` (T5): 10 ms interval; ≥2 stub
  requests within 150 ms; exactly 1 ingest job total (watch_state dedups
  across ticks); after `handle.abort()` the request count is stable over a
  further 60 ms. **RED evidence**: compile failure before implementation —
  `no spawn_watcher in watch` (E0432).
- `restart_resumes_without_rescan` (T6, SRCC-02 headline): file-backed SQLite
  via the production `boot_open` sequence on a tempdir `data_dir` (asserted
  `rt.db_path` is under the tempdir — an in-memory DB cannot fake the pass).
  poll ⇒ 1 job; `drop(store)`/`drop(vault)`; reopen same paths ⇒ poll returns
  0, job count unchanged, `last_poll_at` strictly advanced; then a new stub
  file enqueues exactly 1 more (resume is live, not frozen). This test
  verifies 04-05's durable-state semantics end-to-end and was GREEN on write
  (proof test, not a new-behavior test — the new-behavior RED was T5's
  compile failure).

## Verification

- `cargo test -p saussunet-core` → **256 passed, 0 failed** (baseline 254 + 2).
- `cargo test -p saussunet` (binary crate) → 0 tests, 0 failed (binary crate
  carries no test targets; boot wiring is exercised by the release-binary
  smoke below).
- `cargo run -- --help` exits 0 (no new subcommand — D-12).
- Release binary in an empty dir: same three boot lines as before, no
  `watch ready:` line (zero watch entries).
- `bash scripts/check.sh` → check: OK (incl. clippy -D warnings, release build).
- Acceptance greps: `tokio::time::interval` present in watch/mod.rs;
  `tracing::warn` appears ONLY inside `spawn_watcher` (line 340), not in
  poll_once; `spawn_watcher` called inside `boot()`; `watch_handles` has 4
  sites (bind + spawn + abort loop).

## Deviations

- Task-boundary split of the test file: both tests were authored up front for
  the RED run; the T6 test was temporarily removed to keep the Task 1 commit
  scoped to spawn_watcher, then restored for the Task 2 commit. No content
  difference.
- T6 uses `boot_open(&rt)` twice (the production boot sequence) instead of
  hand-calling `Store::connect` + `ensure_vault` + `Vault::init_fs` — same
  file-backed guarantee (asserted via `rt.db_path`), less test-side drift
  from production wiring.
- Flake recorded: first full-suite run showed 1 failure in the 2-test
  `pipeline_url` target; three subsequent full runs and three targeted runs
  all green. Consistent with the known parallel-load SQLITE_BUSY flake family
  (pre-existing; not debugged per instructions).

## Decisions

- WARN line carries `source = %src.source_id` + poll_once's Display only —
  no new error formatting in the loop (threat-model constraint; poll_once
  already sanitizes).
- Success path logs DEBUG (plan Task 1 wording "DEBUG trace on success with
  counts") plus a one-time DEBUG "watch poller started" with the interval —
  useful for the Phase 5 status UI debugging, zero cost at default level.
- Watcher block placed after the worker if/else so `queue ready:` / mock
  notice print order is unchanged; watcher print comes after — boot output
  order for zero-watch configs is byte-identical.

## Lessons

- `boot()` is the DEFAULT mode of the binary (no `boot` subcommand) — daemon
  smoke = run the binary with no args in a temp dir + timeout.
- A restart-resume proof written against `boot_open` doubles as a regression
  guard for the whole 04-01..04-05 persistence chain; asserting
  `rt.db_path.starts_with(tempdir)` is the cheap way to prove "file-backed"
  without hand-wiring store internals.
- For a brand-new `pub fn`, the genuine RED is the E0432 unresolved-import
  compile error — record it verbatim before implementing.
