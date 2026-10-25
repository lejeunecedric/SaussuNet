---
phase: 01
status: issues_found
date: 2026-10-25
reviewer: gsd-code-review (ASVS L1)
diff_range: 74d51f2..HEAD
fixed: [C-1, W-1, W-3, W-4, W-5, W-6]
fixed-date: 2026-10-25
---

# Phase 01 Code Review — Workspace Foundations + Vault Writer

Scope: `crates/`, `migrations/`, `scripts/`, `Cargo.toml` (4,833 added lines).
Verified live: `cargo check` clean, `cargo test` all green (writer/gen_hash/reindex/init_vault/acceptance suites), and the Critical finding reproduced against the built binary.

## Critical (must-fix)

### C-1 — Path guard does not reject reserved vault root files; `write-page` can clobber `index.md` / `log.md` / `SCHEMA.md`
- **File:** `crates/saussunet-core/src/vault/mod.rs:90-147` (`resolve_page_path`) + `crates/saussunet-core/src/vault/writer.rs:86` (write flow)
- **Issue:** The T-02-01 guard rejects `..`, absolute paths, and `raw/`, but nothing restricts `rel_path` to the four page directories or rejects the writer-managed root files. Reproduced with the built binary:
  - `saussunet write-page --path index.md --from page.md` → **`index.md` is replaced** by the page (step 7 atomic write lands), then step 9 fails with a misleading "index.md has no '## Concepts' section" and exit 1 — the vault is left broken.
  - `--path SCHEMA.md` → **`SCHEMA.md` is replaced** (same pattern; taxonomy parse fails only afterwards).
  - `--path log.md` would silently replace the append-only log (in my repro it was blocked only because SCHEMA.md was already clobbered, failing the taxonomy read first).
  These files have no frontmatter/gen_hash, and `in_wiki_layer()` is false for them, so D-06/D-07/D-08 protection never engages. Partial-write failure (page lands, then error) also violates the "whole or not at all" posture.
- **Fix:** In `resolve_page_path` (or early in `write_page`), reject any `rel_path` whose first component is not one of `entities|concepts|comparisons|queries` (same list as `reindex::PAGE_DIRS`), or at minimum reject the reserved names `SCHEMA.md`, `index.md`, `log.md`, `log-*.md`. Add a regression test asserting `write_page("index.md", …)` is refused **before any mutation**.

## Warnings

### W-1 — `write_atomic` never fsyncs: durability claim not actually held
- **File:** `crates/saussunet-core/src/vault/atomic.rs:45-58`
- **Issue:** `tokio::fs::write(tmp)` then `rename` with no `sync_all()` on the temp file (and no directory fsync after rename). On power loss, the rename can be durable while the file's data blocks are not — the module doc's "a crash mid-write can only leave a temp file, never a half-written page" (ROADMAP SC2) is only true for process crashes, not machine crashes. `grep fsync crates/` → none.
- **Fix:** Replace `tokio::fs::write` with `File::create(&tmp)` → `write_all` → `file.sync_all()` → rename (optionally `sync_data()` on the parent dir). Personal-scale vault, one fsync per write is cheap.

### W-2 — Section-slug collision can mask a hash (contradicts the documented invariant)
- **File:** `crates/saussunet-core/src/vault/ghash.rs:76-83` (`split_sections` dedup)
- **Issue:** The `-2`/`-3` suffix scheme uses a per-base counter, so a document containing `## Dup`, `## Dup`, `## Dup-2` yields slugs `dup`, `dup-2`, `dup-2` — the third section's insert overwrites the second's in the `BTreeMap`, exactly the masking the doc comment ("one section can never mask another's hash") claims is impossible. A human editing the masked section then goes undetected.
- **Fix:** Loop the suffix until the slug is unused across the whole document (track the full emitted-slug set, not per-base counts).

### W-3 — Human-ADDED sections are invisible to `human_edited` and get silently deleted on overwrite
- **File:** `crates/saussunet-core/src/vault/ghash.rs:240-244`
- **Issue:** `human_edited` only checks keys present in `stored`. A human appending a new `## My Notes` section to an agent page changes no existing hash, so the next agent regeneration overwrites and silently deletes it — in tension with D-06's "no silent overwrite, ever" intent (the asymmetry is deliberate for agent-added sections, but the two cases are indistinguishable as implemented).
- **Fix:** Persist the full key set (e.g. a `__section_slugs__` entry or `__section_count__` in the map) at write time; flag any current key absent from the recorded set as a human addition.

### W-4 — `read_to_string(...).ok()` swallows non-NotFound errors → protection bypass
- **File:** `crates/saussunet-core/src/vault/writer.rs:89`
- **Issue:** `let existing = tokio::fs::read_to_string(&target).await.ok();` treats ANY read failure (EACCES, EIO, EISDIR) as "file does not exist". An existing protected / human-owned / gen_hashed page that is momentarily unreadable is then treated as a first write and overwritten (rename needs only directory write permission, so the overwrite succeeds).
- **Fix:** `match read { Ok(s) => Some(s), Err(e) if e.kind() == NotFound => None, Err(e) => return Err(Io) }`.

### W-5 — `connect_memory` pool of 5 over `sqlite::memory:` = one private empty DB per connection
- **File:** `crates/saussunet-core/src/store/mod.rs:66-77`
- **Issue:** Each pooled connection to `:memory:` gets its OWN database; migrations land on whichever connection ran them. Sequential tests reuse the single idle connection so everything passes today, but any concurrent store access (e.g. the 20-way concurrent-writes test, or Phase 2 workers) can open a second connection with an empty schema → intermittent "no such table". sqlx's documented guidance is to pin in-memory pools to one connection.
- **Fix:** `.max_connections(1)` in `connect_memory`, or use `sqlite://file::memory:?cache=shared`.

### W-6 — `reindex` runs without the write lock and outside a transaction
- **File:** `crates/saussunet-core/src/reindex.rs:28-46`
- **Issue:** `reindex` never takes `vault.write_lock`, and `clear_pages` + N `upsert_page` calls are separate statements. An in-process concurrent `write_page` (Phase 3+ generators) can interleave: its registry row is wiped by the clear, or the reindex re-adds a page the writer just deleted. A crash mid-reindex leaves a partial registry (rebuildable, so not data loss — but the projection is inconsistent until the next run).
- **Fix:** Acquire `vault.write_lock` for the whole scan+rebuild, and wrap clear+upserts in one `sqlx::Transaction`.

## Info

### I-1 — Dead error variant and unused dependencies
- **Files:** `crates/saussunet-core/src/error.rs:31-32` (`CoreError::Conflict` is never constructed anywhere); `Cargo.toml:29` `pulldown-cmark` and `Cargo.toml:32-33` `tracing` are declared for `saussunet-core` but never used in `src/`. Drop them (or use them) to keep the dependency surface minimal per INFR-01's spirit.

### I-2 — `created` date copied from existing file after validation
- **File:** `crates/saussunet-core/src/vault/writer.rs:216-226`
- `validate_page` runs on the incoming frontmatter, but step 5 then overwrites `fm.created` with the on-disk value without re-validation. On the scoped-out non-wiki-layer no-gen_hash path, a human-corrupted `created: banana` can be persisted into the new page. Re-validate after normalization, or fall back to `today` when the stored value fails `NaiveDate` parsing.

### I-3 — `frontmatter_hash` folds all unparseable YAML to `Null`
- **File:** `crates/saussunet-core/src/vault/ghash.rs:140-144`
- `unwrap_or(Value::Null)` means two different malformed frontmatters hash identically. Currently unreachable in the conflict path (unparseable on-disk frontmatter routes to the D-08 refuse branch before `human_edited` runs), but the silent fold-over is fragile — hash the raw text bytes on parse failure instead.

### I-4 — `unreachable!()` panic in library code
- **File:** `crates/saussunet-core/src/vault/writer.rs:45` (`index_section`)
- Guarded today by `validate_page`, but a panic path in an embeddable library (QUAL-05: MoriNet will embed this crate) is harsher than returning `CoreError::Frontmatter`.

### I-5 — Log rotation is two separate atomic writes
- **File:** `crates/saussunet-core/src/vault/log.rs:82-98`
- Crash between the archive write and the fresh-log write leaves `log.md` unrotated; the next rotation appends the same entries to `log-YYYY.md` again (duplicate archive entries). Acceptable at personal scale; note it.

### I-6 — Minor robustness/cosmetics
- `index.rs:57-61`: section-end scan matches `## ` inside fenced code blocks (same class as the ghash heading scan; fine for writer-maintained files).
- `writer.rs:34` `sanitize_for_nav` neutralizes newlines and `## [` but not `]]`/leading `- ` in titles for index entries — cosmetic.
- `write-page` accepts extension-less paths (`concepts/foo`), which reindex (`.md`-only scan) will never re-register → registry drift.
- `main.rs`: `write-page` maps path-guard rejections to exit 1 (same as validation errors); consider a distinct code for operator path mistakes.

## Verified good (no action)

- **SQL injection:** every query is bound-parameter (`?`); no `format!`-built SQL anywhere in `store/` (T-01-03 satisfied). The only `format!` is the sqlite URL from local config.
- **SQLite hygiene:** WAL + `busy_timeout(5s)` + `foreign_keys(true)` per connection on both connect paths; FK `ON DELETE CASCADE` is therefore live; `pending_proposals.reason` CHECK matches the writer's three reasons exactly.
- **gen_hash self-hash rule (T-03-04):** `gen_hash` stripped on BOTH sides (write side hashes pre-map draft; check side strips the embedded map) — verified by `gen_hash_key_never_hashes_itself` and the round-trip tests.
- **Symlink handling:** writer rejects symlink targets pre-canonicalize AND catches symlinked-dir escapes via canonicalize-before-check (both tested); reindex skips lstat-symlinks and re-checks containment (tested).
- **Concurrency:** single `tokio::sync::Mutex` per vault held across the whole `write_page` critical section (async lock, so holding across await is correct); `Vault` deliberately non-Clone; 20-way concurrent-write test passes.
- **Log/index injection (T-02-05):** `sanitize_for_nav` blocks forged `## [` entries (tested with crafted title).
- **T-01-01:** DB/config-inside-vault refused at boot (tested).
- **D-01:** no note content in SQLite except `pending_proposals.proposed_content`, which is explicitly justified in the migration comment (it is refused-write state, not a vault projection).
- **sqlx 0.8 pin** documented with the MoriNet coexistence rationale; zero axum/HTTP in core, guarded by `scripts/check.sh`.

## Summary

| Severity | Count |
|---|---|
| Critical | 1 |
| Warning | 6 |
| Info | 6 |

## REVIEW COMPLETE

status: issues_found — critical: 1, warnings: 6, info: 6
