# Phase 1: Pattern Mapping

**Analog codebase:** `/home/c/Projects/Dev/MoriNet` (single crate, axum 0.8 + sqlx 0.8.6 + SQLite + git2)
**Date:** 2026-10-25
**Method:** `gsd-sdk query agent-skills gsd-pattern-mapper` returns `""` on this install (expected) — fell back to manual role classification + analog excerpt extraction per planner-subagent-prompt guidance.

> **Key finding up front:** MoriNet is a *pattern donor, not a template*. Three Phase-1 requirements have **no working analog** in MoriNet and must be established fresh here: (a) **WAL pragmas** — MoriNet never sets them (grep for `pragma|wal|journal_mode` over `src/` → zero hits); (b) **atomic temp+rename writes** — MoriNet writes vault files with plain `tokio::fs::write` (`src/server.rs:773`); (c) **thiserror typed errors** — MoriNet is `anyhow`-only end to end. Details in §2 and §4.

---

## 1. Planned Phase-1 Files — Role Classification

Derived from `01-CONTEXT.md` (D-01..D-08, integration points) and `ARCHITECTURE.md` §Recommended Project Structure, scoped to Phase 1 (no connectors, no LLM, no HTTP).

| Planned file | Role | Data-flow position | Analog strength in MoriNet |
|---|---|---|---|
| `Cargo.toml` (workspace root) | build config | — | Weak (MoriNet is single-crate; see §3) |
| `crates/saussunet-core/Cargo.toml`, `crates/saussunet/Cargo.toml` | build config | — | Weak (no workspace precedent) |
| `migrations/0001_full_schema.sql` (+ splits) | schema / DDL | persistence | **Strong** — `migrations/*.sql` |
| `crates/saussunet-core/src/lib.rs` | public surface | — | Strong — `src/lib.rs` |
| `crates/saussunet-core/src/config.rs` | config loader | input boundary | **Strong** — `src/config.rs` |
| `crates/saussunet-core/src/error.rs` | error taxonomy | cross-cutting | **None** (gap — MoriNet has no thiserror) |
| `crates/saussunet-core/src/store/mod.rs` | pool init + migrate | persistence core | **Strong** — `src/store.rs:84-111` (minus WAL, see gap) |
| `crates/saussunet-core/src/store/vaults.rs` | registry CRUD | persistence | Strong — `store.rs` `seed_vault`/`list_*` idiom |
| `crates/saussunet-core/src/store/pages.rs` (`page_registry`) | projection CRUD | persistence | Strong — same idiom |
| `crates/saussunet-core/src/store/entities.rs` (`entity_registry`) | projection CRUD | persistence | Strong — same idiom |
| `crates/saussunet-core/src/store/proposals.rs` (`pending_proposals`) | conflict recorder | persistence | Strong — same idiom (insert-only) |
| `crates/saussunet-core/src/store/jobs.rs`, `store/sync_state.rs` | schema + minimal APIs (D-02, behavior deferred) | persistence | Medium — table exists; claim logic is Phase 2+ |
| `crates/saussunet-core/src/vault/mod.rs` | `Vault` struct, single write lock, path keying by `vault_id` (D-04) | FS boundary | Medium — MoriNet has no vault writer; use `sources.rs` path discipline |
| `crates/saussunet-core/src/vault/atomic.rs` (or inline in writer) | atomic temp+rename write | FS boundary | **None** (gap — must be new) |
| `crates/saussunet-core/src/vault/frontmatter.rs` | YAML frontmatter split/parse/serialize, `gen_hash` map (D-05) | FS boundary | **Strong** — `src/obsidian/frontmatter.rs` |
| `crates/saussunet-core/src/vault/writer.rs` | sole writer: gen_hash three-way check, `protected:` refusal, no-gen_hash refusal (D-05..D-08), conflict row + log entry | FS + persistence | Medium — logic is new; loud-failure posture from `sources.rs` |
| `crates/saussunet-core/src/vault/schema.rs` | SCHEMA.md seed (llm-wiki conventions) | FS init | Weak — content is llm-wiki skill, not MoriNet |
| `crates/saussunet-core/src/vault/index.rs` | index.md init/sections | FS init | Weak |
| `crates/saussunet-core/src/vault/log.rs` | log.md append (+ rotation rule) | FS append | Weak |
| `crates/saussunet/src/main.rs` | clap CLI (`init`, `init-vault`/`serve`-equivalent), wiring: config → store → vault init | process entry | **Strong** — `src/main.rs` |
| `config.toml` (example at repo root) | config template | input | **Strong** — MoriNet root `config.toml` + `lib.rs::default_config()` |
| `scripts/check.sh` | quality gate | dev tooling | **Strong** — `scripts/check.sh` |
| `crates/saussunet-core/tests/` + per-module `#[cfg(test)]` | temp-vault fixtures | test | Medium — MoriNet uses `temp_dir()+process::id()`, no `tempfile` crate |

---

## 2. Analog Excerpts (concrete, with file:line)

### 2.1 SQLite pool init + migration wiring
**Analog:** `MoriNet/src/store.rs:89-102` — the exact shape to copy, **plus the WAL pragmas MoriNet is missing**.

```rust
// MoriNet src/store.rs:89-102 (as-is — NO WAL, do not copy verbatim)
pub async fn connect(db_path: &Path) -> anyhow::Result<Self> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let url = format!("sqlite://{}?mode=rwc", db_path.display());
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    Ok(Self { pool })
}
```

SaussuNet must upgrade to `SqliteConnectOptions` to satisfy INFR-02 (WAL). Concrete target:

```rust
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use std::str::FromStr;

let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}?mode=rwc", db_path.display()))?
    .create_if_missing(true)
    .journal_mode(SqliteJournalMode::Wal)
    .busy_timeout(std::time::Duration::from_secs(5))
    .foreign_keys(true);
let pool = SqlitePoolOptions::new().max_connections(5).connect_with(opts).await?;
sqlx::migrate!("../../migrations").run(&pool).await?; // path relative to the CORE crate dir
```

Two traps: (a) `sqlx::migrate!()` resolves relative to the crate that calls it — with `migrations/` at the **workspace root** and the call inside `crates/saussunet-core`, the macro path must be `"../../migrations"` (compile-time embedding) — verify at implementation time; (b) `foreign_keys(true)` matters because Phase-1 tables use `REFERENCES ... ON DELETE CASCADE` (see 2.2) and SQLite defaults FK enforcement OFF per connection.

Also copy the test-store idiom (`store.rs:104-111`): `connect_memory()` running the same migrations against `sqlite::memory:`.

### 2.2 Migration file style
**Analog:** `MoriNet/migrations/0002_vaults.sql` — `CREATE TABLE IF NOT EXISTS`, `INTEGER PRIMARY KEY AUTOINCREMENT`, `TEXT NOT NULL DEFAULT (datetime('now'))`, explicit `REFERENCES ... ON DELETE CASCADE`, one logical concern per numbered file (`0001_users.sql` … `0008_shares_stats.sql`). SaussuNet Phase 1 creates the FULL schema up front (D-02): `vaults`, `page_registry`, `entity_registry`, `jobs`, `sync_state`, `pending_proposals` — every table carries `vault_id INTEGER NOT NULL REFERENCES vaults(id)` (D-03). Suggested split: `0001_vaults.sql`, `0002_registries.sql`, `0003_jobs_sync_state.sql`, `0004_pending_proposals.sql`.

### 2.3 Store API idiom (per-table modules)
**Analog:** `MoriNet/src/store.rs` — `sqlx::query_as::<_, (T,)>` with tuple destructuring, `.bind()` chains, `fetch_optional(&self.pool)`, idempotent upsert-by-lookup (`seed_vault`, store.rs:127-163: SELECT then INSERT/UPDATE, no `ON CONFLICT`). Return `anyhow::Result` at MoriNet; **SaussuNet core should return `crate::error::Result<T>` with thiserror** (STACK: "thiserror in the lib, anyhow only at binary boundaries"). Keep MoriNet's good habit: typed row structs (`Vault`, `User` at store.rs:14-52) rather than raw tuples for multi-column reads.

### 2.4 config.toml struct + load
**Analog:** `MoriNet/src/config.rs` — the pattern to replicate near-verbatim:
- `FileConfig` with `#[serde(default)]` per section + `#[serde(default = "fn")]` field defaults (config.rs:15-95); every section parses when absent (`[server]\n` alone must load — there's a test for exactly that, config.rs:172-181).
- `load_config(path) -> anyhow::Result<FileConfig>`: `fs::read_to_string` with path-in-error-message, then `toml::from_str` (config.rs:116-121).
- Two-stage design: `FileConfig` (raw TOML) → `resolve()` → `RuntimeConfig` (fully-resolved, env overrides applied) (config.rs:97-156). SaussuNet needs the same split for `[server].bind` (localhost default, D/INFR-01), `[storage].data_dir` (default `./data`), and a `[vault]` section carrying the single v1 vault id + name (D-04).
- Env-var tests use a `static ENV_LOCK: Mutex<()>` guard because cargo tests share process env (config.rs:162-164) — copy this if SaussuNet reads env overrides.
- `lib.rs::default_config()` returns the example TOML as a `static str`, and `main init` writes it refusing overwrite unless `--force` (main.rs:52-64 + lib.rs tests assert the template parses as valid TOML).

### 2.5 Atomic temp+rename write — **NO ANALOG; establish fresh**
MoriNet writes vault-adjacent files with plain `tokio::fs::write` (`server.rs:773`) — no temp+rename anywhere in the codebase (grep `rename|tmp|temp` finds only test fixtures and HTTP rename routes). CONTEXT/PITFALLS 6 make this a hard requirement ("retrofit = rewrite"). Concrete pattern for `vault/atomic.rs`:

```rust
pub async fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().context(...)?;
    std::fs::create_dir_all(dir)?;                    // vault subdirs may not exist yet
    let tmp = dir.join(format!(
        ".{}.tmp-{}",
        path.file_name().unwrap().to_string_lossy(),
        uuid::Uuid::new_v4()                          // concurrent-safe unique temp name
    ));
    tokio::fs::write(&tmp, bytes).await?;             // SAME directory as target → rename is atomic on POSIX
    tokio::fs::rename(&tmp, path).await?;
    Ok(())
}
```
Key invariants: temp file in the **same directory** (cross-device rename is not atomic), dot-prefixed name so Obsidian/glob scans ignore leftovers, cleanup of the temp on rename failure. All vault mutations funnel through this + the single writer lock (vault/mod.rs).

### 2.6 Frontmatter split/parse
**Analog:** `MoriNet/src/obsidian/frontmatter.rs:1-100` — hand-rolled llm-wiki YAML subset: `split(md) -> (Frontmatter, &str)` returning `(empty, whole input)` on unterminated block (never truncates the page — copy that defensive rule verbatim, frontmatter.rs:57-63), `BTreeMap<String, Field>` with `Scalar`/`List`, values stay strings (no YAML typing — a bare date must not become a date object). **Divergence:** SaussuNet's `gen_hash` map (D-05: section slug → hash, nested under frontmatter) exceeds MoriNet's flat scalar/list subset — STACK pins `serde_yml 0.0.13` + optionally `gray_matter 0.3` for the nested map. Recommended: reuse MoriNet's split logic for the `---` boundary, then serde_yml for the YAML body; keep the "malformed frontmatter → treat as absent, never truncate" contract.

### 2.7 clap main skeleton
**Analog:** `MoriNet/src/main.rs` (whole file, 175 lines — safe to read whole). Pattern: `#[derive(Parser)] struct Cli { command: Option<Command> }`; `#[derive(Subcommand)] enum Command { Version, Init { out, force }, Serve { config, vault } }`; `fn main() -> anyhow::Result<()>` with sync subcommands inline and the async one behind an explicit `tokio::runtime::Builder::new_multi_thread().enable_all().build()?.block_on(serve(...))` (main.rs:66-70). `serve()` order to mirror exactly (main.rs:75-90): load config → resolve → `data_dir` from config → `Store::connect(&data_dir.join("saussunet.db"))` → idempotent bootstraps (`ensure_admin`-style: `ensure_vault_row`, `init_vault_fs`) → serve. SaussuNet Phase 1 subcommands: `init` (config template), `init-vault` / first-boot vault creation (SC1: "running the binary creates a vault"), plus whatever minimal surface drives writer proposals pre-Phase-5 (planner's discretion per CONTEXT).

### 2.8 Loud-failure posture
**Analog:** `MoriNet/src/sources.rs:40-66` — subprocess `NotFound` gets a dedicated `anyhow::bail!` with install instructions; a "ran but produced nothing" state also fails loudly with an explicit comment ("a silent fallback surfaces later as an undebuggable 404"). Apply the same posture to: missing/corrupt SCHEMA.md seed, rename failure, migration failure. Never swallow vault-write errors.

### 2.9 Error taxonomy — **NO ANALOG; establish fresh**
MoriNet uses `anyhow` in the lib too (zero `thiserror` references in `src/` + `Cargo.toml`). Per STACK pin (thiserror 2.0.21 / anyhow 1.0.104): `saussunet-core` defines `pub enum CoreError { #[error(...)] Config(...), Store(sqlx::Error), Io { path, source }, Vault(...), Conflict(...) }` + `pub type Result<T>`; `crates/saussunet/src/main.rs` keeps `anyhow::Result<()>` at the boundary (MoriNet's main.rs already proves that half).

### 2.10 scripts/check.sh
**Analog:** `MoriNet/scripts/check.sh` (19 lines): `set -euo pipefail` + `cd "$(dirname "$0")/.."` → `cargo fmt --check` → `cargo clippy --all-targets -- -D warnings` → `cargo test` → `cargo build --release`. Copy as-is; CI is deliberately local in this project family.

---

## 3. Workspace Cargo.toml Conventions

**MoriNet has no workspace** — single `[package]` with `[lib]`+`[[bin]]` in one crate (`MoriNet/Cargo.toml`), so there is **no `workspace.dependencies` precedent to copy**. SaussuNet establishes it. Conventions to carry over from MoriNet's manifest:

- `edition = "2021"`, `license = "MIT OR Apache-2.0"`, full package metadata block.
- `[profile.release] opt-level = 3, lto = "thin", codegen-units = 1, strip = true` + `[profile.dist] inherits = "release"` (cargo-dist convention).
- Pinned majors from MoriNet's manifest/lock to keep future `saussunet-core`↔MoriNet type-compat: `sqlx 0.8` **locked at 0.8.6** in MoriNet's `Cargo.lock` (⚠️ STACK flags sqlx 0.9.0 vs MoriNet 0.8 — two majors cannot coexist in one binary; MoriNet lib integration is QUAL-05/v2, so the planner may choose 0.9 now with a migration note, or pin 0.8 for free future interop — this is the open sqlx decision in CONTEXT "Claude's Discretion"). `axum 0.8`, `tokio 1.x` (`features = ["macros", "rt-multi-thread"]` — add `"fs"]` for vault writes if using tokio::fs), `clap 4` `features = ["derive"]`, `serde` derive, `toml 0.8`, `uuid v4`, `sha2`, `anyhow 1`.
- `[workspace.dependencies]` should single-source tokio/sqlx/serde per STACK ("constrain via [workspace.dependencies]"); crates/saussunet-core declares zero axum (Anti-Pattern 1 — add a review/CI check).
- **vendored-openssl:** MoriNet needs it only for `git2 = { features = ["vendored-openssl"] }` (git2 0.19 there; STACK pins git2 0.21). **Phase 1 has no git dependency** (git auto-commit is deferred to v2) → do not pull git2/openssl into Phase 1 at all. If added later, follow MoriNet's vendored-openssl choice for static-binary deploys.
- New Phase-1 deps with no MoriNet analog: `serde_yml 0.0.13` (frontmatter YAML), optionally `gray_matter 0.3`, `pulldown-cmark 0.13` (`##` section scan for gen_hash), `thiserror 2`, `tracing`/`tracing-subscriber 0.3`.

---

## 4. Pitfalls Observed in MoriNet (to avoid)

1. **Monolithic modules.** `server.rs` = **5,715 lines**, `store.rs` = **2,975 lines** — the exact anti-pattern ARCHITECTURE.md calls out (Anti-Pattern 6). Do: `store/` split per table from day one (2.3), `vault/` split per concern (writer/index/log/frontmatter), handlers thin.
2. **No WAL pragmas.** MoriNet opens SQLite with a bare URL and default journal — INFR-02 requires WAL; use `SqliteConnectOptions` (§2.1). Also set `busy_timeout` (single-writer + UI reads will contend) and `foreign_keys(true)` (FKs are declared in migrations but unenforced by default in SQLite).
3. **No atomic writes.** Plain `fs::write` to live files (§2.5). A crash mid-write corrupts a vault page — unacceptable when FS is the sole truth (D-01).
4. **anyhow in the library.** MoriNet's lib errors are untyped `anyhow` — a future MoriNet dependency on `saussunet-core` cannot match on error kinds (e.g. "conflict recorded" vs "IO failed"). thiserror in core (§2.9).
5. **`sqlx::migrate!` path semantics change with a workspace.** In MoriNet the macro sits in the crate root next to `migrations/`; in SaussuNet the call lives in `crates/saussunet-core` while `migrations/` is at workspace root — the relative path must be adjusted or the build silently embeds nothing/fails. Verify with a test that a fresh DB gets all six tables.
6. **Test fixtures use `std::env::temp_dir() + process::id()`** (config.rs:174, webdav.rs:428) — collides across parallel tests sharing a process id pattern and never isolates per-test. Prefer the `tempfile` crate (or at least per-test unique dirs + cleanup) for the temp-vault fixtures ARCHITECTURE.md prescribes.
7. **Env-var tests need a mutex** (`ENV_LOCK`, config.rs:162-164) — cargo tests share process env. Copy the pattern if any config value takes an env override.
8. **Idempotent upsert via SELECT-then-INSERT/UPDATE** (`seed_vault`) works but races under concurrency; with the single-writer rule it's fine, but `INSERT ... ON CONFLICT(vault_id, path) DO UPDATE` is cleaner for `page_registry`/`entity_registry` refresh.
9. **Version pin drift risk:** MoriNet manifest says `sqlx = "0.8"`, lock has 0.8.6; STACK pins 0.9.0. Whatever the planner picks, record it in CONVENTIONS and pin in `[workspace.dependencies]` — the two-major collision is flagged as an immediate type error, not a subtle one.
10. **Good posture worth copying:** loud failure with actionable messages (§2.8); "malformed frontmatter → treat as absent, never truncate the page" (frontmatter.rs); config template shipped in `lib.rs::default_config()` with a test asserting it parses; `check.sh` as the single local quality gate.

---

## 5. Mapping Summary Table (file → analog → action)

| SaussuNet Phase-1 file | MoriNet analog | Action |
|---|---|---|
| `migrations/0001..0004` | `migrations/0002_vaults.sql` | copy style; add `vault_id` FK everywhere (D-03) |
| `core/src/store/mod.rs` | `store.rs:89-111` | copy + **add WAL/busy_timeout/FK options** |
| `core/src/store/{vaults,pages,entities,proposals,jobs,sync_state}.rs` | `store.rs` method idiom | split per table; thiserror returns |
| `core/src/config.rs` | `config.rs` (whole) | copy FileConfig→RuntimeConfig two-stage + serde defaults |
| `core/src/error.rs` | — | **new** (thiserror taxonomy) |
| `core/src/vault/mod.rs` | — (path discipline from `sources.rs`) | **new**: VaultId-keyed paths `data/vaults/<id>/`, write lock |
| `core/src/vault/atomic.rs` | — | **new**: temp+rename per §2.5 |
| `core/src/vault/frontmatter.rs` | `obsidian/frontmatter.rs` | copy split contract; serde_yml for nested `gen_hash` map |
| `core/src/vault/writer.rs` | loud-failure posture from `sources.rs:40-66` | **new**: D-05..D-08 three-way merge + conflict rows |
| `core/src/vault/{schema,index,log}.rs` | — | **new**: llm-wiki skill conventions (external canonical ref) |
| `core/src/lib.rs` | `lib.rs` | module list + `version()` + `default_config()` |
| `saussunet/src/main.rs` | `main.rs` (whole) | copy clap skeleton + serve() wiring order |
| `config.toml` | root `config.toml` + `default_config()` | template + `[vault]` single-entry section |
| `scripts/check.sh` | `scripts/check.sh` | copy verbatim |
| `tests/` | scattered `#[cfg(test)]` | temp-vault fixtures via `tempfile` (pitfall 6) |

*Pattern mapping for Phase 1 — Workspace Foundations + Vault Writer. Analog: MoriNet @ /home/c/Projects/Dev/MoriNet, 2026-10-25.*
