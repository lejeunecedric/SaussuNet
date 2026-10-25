---
phase: 01
status: passed
score: 5/5
date: 2026-10-25
verified_by: GSD phase verifier (empirical: code reads + live command runs)
---

# Phase 01 Verification: Workspace Foundations + Vault Writer

**Goal under test:** "A self-hosted binary owns a standards-compliant llm-wiki vault through a single atomic writer, with human edits protected and the schema ready for many vaults."

**Verdict: PASSED — 5/5 success criteria empirically verified.** Final gate `bash scripts/check.sh` → `check: OK` (fmt, clippy `-D warnings`, 76 tests, axum-absence guard, release build).

## Success Criteria

### SC1 — Binary creates a standards-compliant vault — ✅ VERIFIED
Ran the release binary in a fresh temp dir per README Quickstart (`init-vault` → `write-page --path concepts/demo.md --from demo.md`):
- Created `data/vaults/1/` with `SCHEMA.md`, `index.md`, `log.md`, and the immutable `raw/` tree (`raw/articles`, `raw/papers`, `raw/transcripts`, `raw/assets`). `init-vault` is idempotent (ran twice, exit 0).
- Written page carries full YAML frontmatter (`title/created/updated/type/tags/sources` + writer-managed `gen_hash` map) and 2 outbound wikilinks (`[[alpha]]`, `[[beta]]`); writer rejects pages with <2 wikilinks (`writer.rs:98`, test `rejects_page_with_one_wikilink`).
- `index.md` gained the entry under `## Concepts` with header counters; `log.md` gained `## [2026-10-25] create | Demo Page`.
- `write-page --path raw/articles/x.md` refused: `raw/ is the immutable evidence layer; page writes are refused (Pitfall 4)` (exit 1).
- Output is plain Obsidian-compatible Markdown per llm-wiki conventions (final open-in-Obsidian check listed under Human Verification).

### SC2 — Sole author, every write atomic temp+rename — ✅ VERIFIED
- Grep audit: `grep -rn "fs::write|File::create" crates/saussunet-core/src/` → only `vault/atomic.rs:45` (`tokio::fs::write` of the temp) plus one test fixture at line 126. No write path outside `atomic.rs`.
- `atomic.rs` semantics: dot-prefixed uuid temp **in the target's own directory** (same-filesystem rename ⇒ atomic), `tokio::fs::rename` as last step, temp removed on any failure path; `append_lines` = read-whole + concat + `write_atomic` (no bare append). Crash mid-write can only leave a temp, never a half-written page.
- `writer.rs` is the sole mutation entry (`write_page`), serialized by a per-vault `write_lock` (test `twenty_concurrent_writes_are_serialized_and_clean`); E2E vault scan found 0 `.tmp-*` files.

### SC3 — Human edit survives regeneration — ✅ VERIFIED
- `cargo test -p saussunet-core --test gen_hash` → **7 passed, 0 failed** (`human_edit_survives_regeneration`, `frontmatter_only_human_edit_routes_to_conflict`, `protected_page_refuses_all_writes`, `human_owned_page_without_gen_hash_is_refused`, `malformed_frontmatter_under_entities_is_human_owned`, `clean_regeneration_overwrites_and_rehashes`, `conflicts_listing_matches_cli_contract`).
- Live E2E: hand-edited the generated page, re-ran `write-page` → `conflict recorded: concepts/demo.md (gen_hash_mismatch)`; edit still on disk (`grep -c "HUMAN EDIT HERE"` → 1); `pending_proposals` row `(1, 1, 'concepts/demo.md', 'gen_hash_mismatch', 'pending')`; `log.md` gained `## [2026-10-25] conflict | Demo Page` with proposal id.

### SC4 — Vault id in config, schema, paths, queues — ✅ VERIFIED
- Migrations: `page_registry`, `entity_registry`, `jobs`, `sync_state`, `pending_proposals` all carry `vault_id INTEGER NOT NULL REFERENCES vaults(id)`; only `vaults` itself uses `id` (confirmed via `PRAGMA table_info` on the live DB).
- Path derivation `config.rs:107`: `data_dir.join("vaults").join(vault.id)` → E2E produced `data/vaults/1/`; config tests assert `vaults/1` and `vaults/7`.
- Store APIs are vault-keyed (`vault_id` first param on `upsert_page`, `list_jobs`, `upsert_sync_state`, `resolve_entity`, `insert_proposal`, …); jobs row invisible under vault 2 (reindex + acceptance tests).
- Note (non-gap): the id is a primitive `i64` rather than a `VaultId` newtype; the requirement ("carry a vault id") is met.

### SC5 — SQLite WAL, no external services, localhost default — ✅ VERIFIED
- `PRAGMA journal_mode` on the live DB → `wal` (set in `store/mod.rs:48` via `SqliteJournalMode::Wal` + busy_timeout + foreign_keys).
- No external services: binary uses only SQLite + filesystem; `check.sh` guards axum absence in `saussunet-core`.
- Bind default `127.0.0.1:8080` (`config.rs:73-74`, tests `default_bind_is_localhost` etc.); Phase-1 zero-listener rationale documented in SKELETON.md ("Phase 1 opens NO listener … actual bind arrives with the Phase 5 UI") and README Decisions.

## Requirements Traceability

| Requirement | Code | Tests / evidence | Verdict |
|---|---|---|---|
| INFR-01 | Workspace `crates/saussunet-core` (zero HTTP deps, Cargo.toml) + `crates/saussunet` binary | `check.sh` axum-absence guard; release build OK | ✅ |
| INFR-02 | `store/mod.rs` WAL pragmas; `config.rs` bind default; SQLite + FS only | `PRAGMA journal_mode → wal` (live); config tests; zero-listener acceptance block | ✅ |
| VLET-01 | `writer.rs` (frontmatter validate, wikilink floor, index/log), `schema.rs`, `init_vault` raw/ tree | writer.rs tests (7), init_vault tests (5), E2E vault dump | ✅ |
| VLET-02 | `atomic.rs` sole write site; `write_page` sole entry + per-vault lock | grep audit (only atomic.rs), concurrent-writes test, 0 temps in vault | ✅ |
| VLET-03 | `ghash.rs` three-way merge; `writer.rs` D-06 branch | gen_hash tests (7) + live conflict E2E | ✅ |
| VLET-04 | migrations vault_id everywhere; `config.rs` vault_root; vault-keyed store APIs | PRAGMA table_info dump, reindex vault-isolation tests, acceptance SC4 block | ✅ |

## Decisions Audit

| Decision | Evidence | Verdict |
|---|---|---|
| D-01 SQLite never stores note content | `page_registry` is metadata-only; `reindex` rebuilds it (6 tests + live `reindexed: 1 pages` after DB wipe path). `pending_proposals.proposed_content` is a documented, justified exception (migration 0004 header: record of a refused write, exists nowhere else) | ✅ |
| D-02 Full schema in Phase 1 | All six tables exist after first boot (`_sqlx_migrations` + vaults, page_registry, entity_registry, jobs, sync_state, pending_proposals) | ✅ |
| D-03 One SQLite DB | `config.rs:106` → `data/saussunet.db`; single DB observed in E2E | ✅ |
| D-04 Paths keyed by vault id | `config.rs:107`; E2E `data/vaults/1/`; config tests for ids 1 and 7 | ✅ |
| D-05 `##` sections, per-section map in frontmatter, frontmatter hashed separately | `ghash.rs:28-37` (`^## ` split, `__preamble__`); strip-gen_hash-before-frontmatter-hash on BOTH sides (`strip_gen_hash` read side + `writer.rs:234-237` draft serialized without map); test `gen_hash_key_never_hashes_itself` | ✅ |
| D-06 Mismatch → skip + record + log | `writer.rs:150-178`; live E2E produced conflict row + log entry, write skipped | ✅ |
| D-07 `protected: true` refuses ALL writes, proposal still recorded | `writer.rs:127-148`; test `protected_page_refuses_all_writes` | ✅ |
| D-08 No gen_hash map in entities//concepts/ = human-owned, refuse + log — incl. malformed frontmatter (parses to None ⇒ authorship unprovable) | `writer.rs:116-118, 182-208`; tests `human_owned_page_without_gen_hash_is_refused`, `malformed_frontmatter_under_entities_is_human_owned` | ✅ |

## Gaps

None.

## Human Verification (cannot be automated here)

1. **Open the generated vault in real Obsidian** — the vault is standard Markdown + YAML frontmatter per llm-wiki conventions (SC1 evidence), but visual confirmation of Obsidian rendering (wikilink graph, frontmatter properties panel) requires the desktop app. Suggested: `obsidian data/vaults/1` on any machine with the release binary's output.

## Notes (non-blocking)

- Store APIs take `vault_id: i64` rather than a `VaultId` newtype; consider a newtype in Phase 2 when the queue layer lands.
- `jobs`/`sync_state` are schema + minimal store APIs only, per D-02 deferral (behavior in Phases 2/4) — as designed, not a gap.
