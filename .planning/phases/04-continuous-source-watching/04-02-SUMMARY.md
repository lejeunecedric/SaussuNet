---
phase: 4
plan: 02
one_liner: "migration 0009 watch_state table + per-file store CRUD + narrower sync_state writers (record_sync_poll/set_sync_token/set_sync_unsupported)"
requirements_completed: [SRCC-02]
---

# Plan 04-02 Summary — migration 0009 + `watch_state` store + narrower sync_state writers

## What was built

- `migrations/0009_watch_state.sql` (new) — verbatim DDL from RESEARCH:
  `watch_state` table (PK `(vault_id, source_id, path)`, columns
  `is_dir/etag/size/sha256/last_seen_at`, FK `vaults(id) ON DELETE CASCADE`)
  + `ALTER TABLE sync_state ADD COLUMN sync_unsupported INTEGER NOT NULL
  DEFAULT 0` (D-05 groundwork). Auto-applies via `sqlx::migrate!` on every
  `Store::connect` — no bootstrap change (T16: full suite green proves it).
- `crates/saussunet-core/src/store/watch_state.rs` (new) — `WatchRow`
  (FromRow) + four `Store` methods:
  - `upsert_watch_row(vault_id, source_id, path, is_dir, etag, size, sha256, last_seen_at)`
    — INSERT..ON CONFLICT(vault_id, source_id, path) overwriting all payload
    columns; the primary per-file write path (crash-safe order: after each
    `insert_job`).
  - `list_watch_rows(vault_id, source_id) -> Vec<WatchRow>` (ORDER BY path).
  - `delete_watch_row(vault_id, source_id, path)` — missing key is a no-op.
  - `replace_watch_rows(vault_id, source_id, &[WatchRow])` — D-11 diff-and-swap
    in ONE transaction (`pool.begin()` → DELETE all for the source → insert
    fresh set → commit); scoped per source, other sources untouched.
- `crates/saussunet-core/src/store/mod.rs` — `mod watch_state;` +
  `pub use watch_state::WatchRow;`.
- `crates/saussunet-core/src/store/sync_state.rs` — `SyncState` gains
  `sync_unsupported: bool` (SELECT list extended); three new upsert-safe
  writers mirroring `set_source_sha256`'s single-column pattern, each creating
  the row when absent and touching ONLY its own columns:
  - `record_sync_poll(vault_id, source_id, last_poll_at, last_error)`
  - `set_sync_token(vault_id, source_id, token)`
  - `set_sync_unsupported(vault_id, source_id, unsupported)`
  `upsert_sync_state` was NOT modified (Phase 3 callers keep its all-column
  semantics; the poller must use the narrow writers — RESEARCH ⚠️ resolved).

## Tests added (all TDD, RED observed before GREEN)

Task 1 (RED: compile-absence — E0422 `cannot find type WatchRow`, E0599
`no method named upsert_watch_row/list_watch_rows/delete_watch_row/replace_watch_rows`):
`watch_row_roundtrip`, `replace_watch_rows_swaps_atomically` (3 seed → replace
with 2 → exactly the new 2; empty-swap clears source, other source survives),
`delete_watch_row_removes_only_its_key` (cross-source scoping + no-op delete).

Task 2 (RED: compile-absence — E0599 `no method named record_sync_poll/
set_sync_token`, E0609 `no field sync_unsupported on SyncState`):
`record_sync_poll_preserves_token_and_sha256` (token/etag/sha256 survive;
last_poll_at/last_error updated; upsert-creates missing row),
`set_sync_token_and_flag_roundtrip` (token set/clear; sync_unsupported
0→1→0 via get_sync_state; other columns untouched; upsert-creates).

Final: `cargo test -p saussunet-core` → **222 passed, 0 failed**
(baseline 217 + 5 new). `bash scripts/check.sh` → check: OK.

## Deviations

- `store/mod.rs` also gained `pub use watch_state::WatchRow;` (plan said only
  `mod watch_state;`) — matches the existing convention of exporting every row
  struct (`SyncState`, `VaultRow`, …) and 04-04/04-05 need the type by name.
- Separate `style(04): cargo fmt` commit: rustfmt reflowed two long
  signatures/lines in the two new/edited store files after the task commits.
- No flaky queue-test failures observed this run.

## Decisions

- `list_watch_rows` returns rows `ORDER BY path` for deterministic diffing and
  test assertions.
- `replace_watch_rows` binds row fields directly (caller supplies
  `vault_id`/`source_id` per row); no validation that rows match the key —
  same trust model as the other store writers.
- `set_sync_unsupported` binds `bool` directly (sqlx maps it to SQLite
  INTEGER); `get_sync_state` reads it back as `bool` via FromRow.

## Lessons

- sqlx `migrate!` embeds at COMPILE time — adding a migration requires a
  rebuild, but zero wiring; the whole existing suite passing IS the T16 proof.
- In-module tests referencing a not-yet-written API give genuine
  compile-absence REDs (E0422/E0599/E0609) — write tests-only file first,
  then paste the impl above them.
- `PoolTransaction` usage: `self.pool.begin()` then `&mut *tx` for each query,
  `tx.commit()` — needed because `Store.pool` is private but submodules of
  `store` can reach it.
