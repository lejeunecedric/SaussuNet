---
phase: 01
plan: 04
subsystem: workspace-foundations-vault-writer
tags: [rust, reindex, rebuildability, vault-id, jobs, sync-state, entity-resolve, acceptance-test, readme, clap]
key-files:
  - crates/saussunet-core/src/reindex.rs
  - crates/saussunet-core/src/store/jobs.rs
  - crates/saussunet-core/src/store/sync_state.rs
  - crates/saussunet-core/src/store/entities.rs
  - crates/saussunet-core/src/store/mod.rs
  - crates/saussunet-core/src/lib.rs
  - crates/saussunet/src/main.rs
  - crates/saussunet-core/tests/reindex.rs
  - crates/saussunet-core/tests/phase1_acceptance.rs
  - README.md
---

# 01-04 Summary: reindex (D-01 proof), vault-keyed store APIs, SC1–SC5 acceptance test, README

## What was built

### Task 1 — reindex + minimal vault-id-keyed store APIs (commit 385e03e)
- `reindex.rs`: `reindex(vault, store) -> Result<u64>` — the D-01 rebuildability
  proof. Canonicalizes the vault root, walks `entities/ concepts/ comparisons/
  queries/` recursively (sync std::fs walk; dot-prefixed entries skipped so
  writer `.{name}.tmp-*` temps are never indexed), reads each `.md`, runs
  `frontmatter::split` + `parse` + minimal validation (title and `type`
  present — full taxonomy validation stays with the writer), then
  `clear_pages(vault.id)` + one `upsert_page` per parsed page (rel path with
  forward slashes, title, page_type, tags_json, updated, `has_gen_hash = 1`
  iff a gen_hash map is present — the D-08 signal in the projection).
  T-04-01: `read_dir` file_type is lstat-based so symlinks are skipped
  outright, plus a canonicalize-and-contain check on every queued file
  (defense in depth). T-04-02: parse/validation failures print
  `skipped <rel_path>: <reason>` to stderr and continue (exit stays 0).
  T-04-03/T-04-05: every store call carries `vault.id`; the clear is the
  existing `WHERE vault_id = ?` scoped DELETE.
- `store/jobs.rs`: `list_jobs(vault_id, status: Option<&str>)` — optional
  status filter, claim order `priority DESC, created_at, id` (deterministic
  tiebreak); doc-comment states claim/claim-expiry/retry are Phase 2 (D-02).
  `insert_job(vault_id, kind, payload_json) -> Result<i64>` already existed.
- `store/sync_state.rs`: unchanged — 01-01 already shipped exactly the plan's
  `upsert_sync_state` (ON CONFLICT(vault_id, source_id) DO UPDATE) /
  `get_sync_state(vault_id, source_id)` surface with the Phase-4 deferral
  doc-comment (verified against the plan's signatures).
- `store/entities.rs`: `resolve_entity(vault_id, name) -> Result<Option<EntityRecord>>`
  — normalized exact match (lowercase, trim, whitespace-collapsed) against
  `canonical_name` OR any alias in `aliases_json` (candidates fetched for the
  vault, matched in Rust — personal-scale, no LIKE). Doc-comment records the
  Phase-3 resolve-before-create contract (Pitfall 2) and that fuzzy/threshold
  matching is explicitly NOT here.
- `store/mod.rs`: re-exported `Job`, `SyncState`, `EntityRecord` (submodules
  are private; CLI/tests must name the row types — same pattern as 01-03's
  proposals re-export).
- `main.rs`: `Reindex` subcommand sharing boot wiring; prints
  `reindexed: N pages`, exit 0.
- `tests/reindex.rs` (6 tests): registry wipe → reindex returns 4 (3 writer
  pages + 1 hand-made page; malformed file skipped and excluded; dot-prefixed
  temp invisible), `has_gen_hash` true only on the 3 writer pages, reindex
  idempotent; two-vault isolation (vault 2 rows survive vault 1 clear+reindex,
  T-04-03); symlink-escape file not indexed (T-04-01); jobs insert/list
  round-trip with vault isolation + status filter; sync_state upsert-twice
  updates token/etag/last_error and is vault-scoped; resolve_entity matches
  canonical (case/whitespace-insensitive) and alias, misses unknown/blank
  names and other vaults.

### Task 2 — SC1–SC5 acceptance test + README quickstart (commit 83cef6a)
- `tests/phase1_acceptance.rs`: one test `phase1_success_criteria_end_to_end`
  on a single fresh temp data dir, five blocks quoting each ROADMAP criterion
  as a comment. SC1: boot creates SCHEMA.md/index.md/log.md/raw-tree; writer
  page on disk has full frontmatter, ≥2 wikilinks, and NO top-level keys
  beyond the documented set {title, created, updated, type, tags, sources,
  confidence, contested, contradictions, protected, gen_hash}. SC2: zero
  `.tmp-*` files anywhere under the vault (recursive scan, re-checked after
  the SC3 writes) and every page file parses. SC3: hand-edit → regenerate →
  `ConflictRecorded(gen_hash_mismatch)`, edit survives, conflict row exists.
  SC4: vault id in resolved RuntimeConfig, `PRAGMA table_info` loop proves
  `id`/`vault_id` on all six tables, `vaults/1` path, jobs row invisible under
  vault 2. SC5: `PRAGMA journal_mode == wal`, resolved bind starts
  `127.0.0.1`, structural zero-listener assertion (boot returns; no server
  started) with the sanctioned planner-discretion rationale cited from
  01-CONTEXT.md/SKELETON.md.
- `README.md`: replaced the stub Development section with a Quickstart
  (build --release, bare/init-vault boot, write-page with inline demo page,
  conflicts, reindex, init, check.sh) and a Decisions section recording the
  sqlx-0.8 pin (MoriNet QUAL-05 rationale, SKELETON link), CLI-only until
  Phase 5, D-01 vault-FS-truth, D-03/D-04 one-vault-multi-ready. Vision
  content untouched; README is 121 lines.

## Commits
| hash | description |
|---|---|
| 385e03e | feat(01-04): reindex rebuildability proof, vault-keyed jobs/sync_state/entity-resolve APIs, reindex CLI |
| 83cef6a | feat(01-04): Phase-1 SC1-SC5 acceptance test and README quickstart with recorded decisions |

## Deviations
1. **`collect_markdown` is a synchronous std::fs walk** (plan implied async):
   recursive `async fn` requires `Box::pin` indirection for no benefit at
   personal scale; the scan is a one-shot CLI operation, not a hot path.
2. **`sync_state.rs` needed no code change** — 01-01 already shipped exactly
   the minimal API this plan specifies (vault-keyed upsert/get, Phase-4
   deferral comment); verified rather than rewritten.
3. **`list_jobs` ordering adds an `id` tiebreak** (`priority DESC, created_at,
   id`): `created_at` has second granularity in SQLite, so same-second inserts
   would otherwise order nondeterministically.
4. **Symlinks are skipped outright by the scan** (lstat-based `file_type`),
   with the canonicalize-and-contain check kept as defense in depth; the plan's
   containment rule is satisfied strictly (even vault-internal symlinks are
   not indexed — conservative, documented in code).
5. **SC2's "writer is the only mutation path" leg is asserted as the
   observable** (zero temps + parseable pages) per the plan's own wording;
   the structural invariant was established in 01-02.

## Test results
- `bash scripts/check.sh` → `check: OK` (fmt, `clippy --all-targets -D warnings`,
  all tests, axum-absence guard, release build).
- `cargo test` → **76 passed, 0 failed** (50 unit + 5 init_vault + 7 writer +
  7 gen_hash + 6 reindex + 1 phase1_acceptance).
- Manual CLI E2E (temp dir): `init-vault` → `write-page` → python `DELETE FROM
  page_registry` (count 1 → 0) → `reindex` printed `reindexed: 1 pages` and
  restored exactly 1 row; second `reindex` idempotent, exit 0.
- README quickstart commands executed verbatim in a clean temp dir with the
  release binary: `init-vault`, `write-page` (inline demo.md), `conflicts`
  (`no pending conflicts`), `reindex` (`reindexed: 1 pages`), `init` — all
  exit 0 as documented.

## Decisions (discretion calls made)
- reindex performs minimal validation only (title + type present); malformed
  pages are lint-style `skipped` lines, never a failed command (T-04-02).
- `resolve_entity` returns the full `EntityRecord` so Phase 3 can see
  `page_path`/`aliases_json` without a second query.
- Acceptance test re-scans for `.tmp-*` after the SC3 block so SC2's "after
  all writes" wording holds literally while keeping the SC1..SC5 block order.
- README keeps the future-stack "Stack" line (axum/rmcp are later phases) and
  adds Phase-1 Decisions separately.

## Lessons
- Recursive `async fn` does not compile without boxing — for a CLI one-shot FS
  scan, a plain `std::fs` recursion inside an async fn is simpler and clippy-clean.
- `tokio::fs::read_dir`/`std::fs::DirEntry::file_type` is lstat-based: symlinks
  report neither file nor dir, so a naive `is_file()` filter already excludes
  them — the canonicalize containment check is belt-and-braces, not the only line of defense.
- serde_yml `Mapping` is String-keyed (01-03 lesson, reconfirmed): `doc.keys()`
  yields `&String`; `key.as_str()` is a plain `&str`, no `expect` needed.
- SQLite `datetime('now')` timestamps are second-granular: any ORDER BY on
  `created_at` needs a unique tiebreaker (`id`) for deterministic claim order.
- `cargo fmt` import ordering: hand-placed `use` lines must sit in rustfmt's
  sorted position or `check.sh` fmt gate fails even when code compiles.
