---
phase: 01
plan: 01
subsystem: workspace-foundations-vault-writer
tags: [rust, cargo-workspace, sqlite, sqlx, wal, llm-wiki, vault-init, clap]
key-files:
  - Cargo.toml
  - config.toml
  - scripts/check.sh
  - migrations/0001_vaults.sql
  - migrations/0002_registries.sql
  - migrations/0003_jobs_sync_state.sql
  - migrations/0004_pending_proposals.sql
  - crates/saussunet-core/src/lib.rs
  - crates/saussunet-core/src/error.rs
  - crates/saussunet-core/src/config.rs
  - crates/saussunet-core/src/store/mod.rs
  - crates/saussunet-core/src/store/vaults.rs
  - crates/saussunet-core/src/store/pages.rs
  - crates/saussunet-core/src/store/entities.rs
  - crates/saussunet-core/src/store/proposals.rs
  - crates/saussunet-core/src/store/jobs.rs
  - crates/saussunet-core/src/store/sync_state.rs
  - crates/saussunet-core/src/vault/mod.rs
  - crates/saussunet-core/src/vault/schema.rs
  - crates/saussunet-core/src/vault/index.rs
  - crates/saussunet-core/src/vault/log.rs
  - crates/saussunet-core/tests/init_vault.rs
  - crates/saussunet/src/main.rs
---

# 01-01 Summary: Walking Skeleton (workspace + WAL store + vault init)

## What was built

### Task 1 — Cargo workspace scaffold + config + error taxonomy + clap binary + quality gate (commit 2272ddc)
- Two-member workspace (`crates/saussunet-core` lib + `crates/saussunet` bin), `resolver = "2"`, `[workspace.package]` edition 2021 / version 0.1.0 / MIT OR Apache-2.0, `[profile.release]` opt-level 3 / lto thin / codegen-units 1 / strip.
- `[workspace.dependencies]` single-sources all Phase-1 deps; **sqlx pinned 0.8** (resolves to 0.8.6, MoriNet-aligned) with a comment citing the rationale. No axum/reqwest/git2/openssl anywhere.
- `error.rs`: thiserror `CoreError` (`Config`, `Store(#[from] sqlx::Error)`, `Io { path, source }`, `Vault`, `Frontmatter`, `Conflict { path, reason }`) + `pub type Result<T>`.
- `config.rs`: MoriNet two-stage pattern — `FileConfig` (`[server].bind` = "127.0.0.1:8080", `[storage].data_dir` = "./data", `[vault].id` = 1 / `name` = "main") → `resolve()` → `RuntimeConfig { db_path, vault_root = data/vaults/<id>, bind, vault_id, vault_name }` (D-03/D-04). `load_config` keeps path in error message; `load_config_or_default` for the bare-run path. Unit tests: bare `[server]` parses, empty doc parses, bind default exact, vault_root ends `vaults/1`, custom id changes path.
- `lib.rs`: module list, `version()`, `default_config()` (root `config.toml` is byte-identical — asserted by test).
- `main.rs`: clap `version` / `init --out --force` (refuses overwrite: stderr "refusing to overwrite", exit 1); bare run stub (replaced in Task 2).
- `scripts/check.sh`: fmt + clippy `-D warnings` + test + axum-absence grep guard + release build. `.gitignore` extended: `/data/`, `config.local.toml`, `*.tmp-*`.

### Task 2 — Full SQLite schema + WAL store + vault FS init + end-to-end boot (commit fdc4d6c)
- Four migrations (D-02 full schema, MoriNet style): `vaults`; `page_registry` + `entity_registry` (pure projections, **no content column** — D-01); `jobs` (+ `idx_jobs_claim`) + `sync_state`; `pending_proposals` with reason CHECK and a SQL comment documenting the `proposed_content` D-01 exception. Every non-vaults table carries `vault_id INTEGER NOT NULL REFERENCES vaults(id) ON DELETE CASCADE` (D-03/VLET-04).
- `Store::connect`: `SqliteConnectOptions` with `journal_mode(Wal)` + `busy_timeout(5s)` + `foreign_keys(true)` + `create_if_missing`, pool max 5, `sqlx::migrate!("../../migrations")` (workspace-root path trap handled; six-table test proves embedding). `connect_memory()` mirrors it (WAL omitted — `:memory:` cannot use it).
- Per-table store modules (§2.3 idiom, all parameterized binds — T-01-03): `ensure_vault` pinned-id upsert `INSERT INTO vaults(id,name) VALUES (?,?) ON CONFLICT(id) DO UPDATE SET name=excluded.name` + read-back id assertion (no AUTOINCREMENT drift); `upsert_page`/`list_pages`/`clear_pages`; `upsert_entity`/`list_entities`; `insert_proposal`/`list_pending_proposals`; `insert_job`/`list_jobs`; `upsert_sync_state`/`get_sync_state`.
- `vault/`: `Vault { id, name, root }` + `init_fs()` creating `raw/{articles,papers,transcripts,assets}` + `entities/ concepts/ comparisons/ queries/` and writing SCHEMA.md / index.md / log.md **only if absent** (never clobbers). Seeds follow the llm-wiki SKILL.md templates: SCHEMA.md with Domain/Conventions/Frontmatter/raw/ Frontmatter/**Tag Taxonomy** (People/Orgs: person, company, organization, place; Topics: technology, science, health, finance, history, product, event; Meta: comparison, timeline, reference, process)/Page Thresholds/page-type sections/Update Policy, plus the SaussuNet `gen_hash` writer-managed and `protected: true` lines (D-05/D-07); index.md with `Total pages: 0` + four sections; log.md with rotation rule + `## [date] create | Wiki initialized`.
- Boot wiring: `saussunet_core::boot(&RuntimeConfig)` (shared by bare run and `init-vault`): T-01-01 check (`assert_outside_vault` on db_path; main also checks the config file path) → `Store::connect` → `ensure_vault` → row-id equality assert → `Vault::init_fs` → canonical root; main prints `vault ready: <abs path>`, exit 0.
- `tests/init_vault.rs` (5 tests): double-boot idempotency + SCHEMA sentinel survival + rename-same-id keeps exactly 1 row / unchanged id / stable `vaults/1` dir; `PRAGMA journal_mode == wal` on file DB; memory DB has all six tables + `PRAGMA foreign_keys` ON + `vault_id` column on every non-vaults table; vault tree + seed content assertions; boot refuses db-inside-vault (T-01-01).

## Commits
| hash | description |
|---|---|
| 2272ddc | feat(01-01): workspace scaffold, config two-stage, thiserror taxonomy, clap binary, quality gate |
| fdc4d6c | feat(01-01): full SQLite schema, WAL store, vault FS init, end-to-end boot |

## Deviations
1. **`tracing` pinned `0.1`, not the plan's "0.3"**: the `tracing` crate's latest line is 0.1.x (STACK's "0.3.23" refers to `tracing-subscriber`, which is 0.3). Plan's `tracing = "0.3"` is unsatisfiable on crates.io; comment added in `[workspace.dependencies]`.
2. **`boot()` lives in `saussunet-core` (lib.rs), not only in main.rs**: the plan's integration test requires driving "boot logic" from `tests/init_vault.rs`; putting the sequence in core lets the test exercise the exact production path. main.rs keeps the boundary (config load, config-placement check, print).
3. **`Vault::new(id, name, root)` takes the name** (plan said `Vault::new(id, root)`): the SCHEMA.md/log.md Domain seed needs the vault name, so it lives on the struct instead of being passed to `init_fs`.
4. **Manual `Default` impls for config sections**: derived `Default` bypasses `#[serde(default = "fn")]` when a whole section is absent, so `FileConfig::default()` (used when config.toml is missing) would have produced empty bind/data_dir. Manual impls keep defaults identical on both paths.
5. **`MigrateError` conversion**: sqlx 0.8's `Migrator::run` returns `MigrateError`, not `sqlx::Error`; wrapped explicitly into `CoreError::Store(sqlx::Error::Migrate(...))`.

## Test results
- `bash scripts/check.sh` → `check: OK` (fmt clean, `clippy --all-targets -D warnings` clean, all tests pass, axum-absence guard passes, release build succeeds).
- `cargo test -p saussunet-core --test init_vault` → 5 passed, 0 failed (idempotent boot + rename, WAL pragma, six tables + FKs + vault_id columns, vault tree/seeds, db-in-vault rejection).
- `cargo test` (full, via check.sh) → all unit + integration tests pass (config defaults, template-parity, seed content, T-01-01 helper).
- Manual: fresh temp data_dir → `./target/release/saussunet` (bare) and `saussunet init-vault` twice → all exit 0, each prints `vault ready: <abs path>`; vault tree matches llm-wiki layout (SCHEMA.md, index.md, log.md, raw/{articles,papers,transcripts,assets}, entities/, concepts/, comparisons/, queries/); DB has exactly 1 vault row; `PRAGMA journal_mode` = `wal`.
- `cargo run -p saussunet -- version` → `saussunet 0.1.0`; `init` twice → exit 0 then exit 1 with "refusing to overwrite" on stderr.

## Decisions (discretion calls made)
- sqlx pinned 0.8 (resolves 0.8.6) per SKELETON.md rationale, recorded as a comment in `[workspace.dependencies]`.
- Pre-UI operability = clap subcommands (`version`, `init`, `init-vault` + bare-run boot); no HTTP in Phase 1.
- `connect_memory()` skips WAL (impossible on `:memory:`) but keeps busy_timeout + foreign_keys so FK-dependent tests behave like production.
- `Store::pool()` exposed as the escape hatch for tests/later phases instead of leaking query APIs through core helpers.
- `assert_outside_vault` uses best-effort absolutization (canonicalize nearest existing ancestor) so the check works before the vault dir exists.

## Lessons
- Derived `Default` + `#[serde(default = "fn")]` are two independent default paths; when a whole section can be absent, both must be implemented or defaults silently diverge.
- `PRAGMA table_info(t)` returns `(cid, name, type, notnull, dflt_value, pk)` — tuple reads must start at `cid`, not `name`.
- sqlx 0.8: `migrate!().run()` errors are `MigrateError`, not `sqlx::Error` — needs explicit conversion when the error enum only has `#[from] sqlx::Error`.
- crates.io reality check beats plan text: `tracing` is 0.1.x (the 0.3 line is `tracing-subscriber`); verify pins resolve before writing code against them.
