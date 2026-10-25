# Phase 2 Patterns: LLM Provider + Durable Job Queue

**Mapped:** 2026-10-25 · **Source of intent:** `.planning/phases/02-llm-provider-durable-job-queue/02-CONTEXT.md` (D-01..D-15), `.planning/ROADMAP.md` §Phase 2, `.planning/research/ARCHITECTURE.md` (Patterns 1–2, recommended `llm/` layout)

Every file Phase 2 creates or modifies, mapped to its closest existing analog with concrete excerpts. Gaps where the repo has **NO ANALOG** are flagged explicitly so the planner establishes those patterns fresh.

> Note: `gsd-sdk query agent-skills gsd-pattern-mapper` returns empty on this install; this document follows the GSD engine template intent (planner-subagent-prompt.md) instead.

---

## A. New files

### A1. `crates/saussunet-core/src/llm/mod.rs` — `LlmProvider` trait + provider factory

- **Role:** Declares the trait surface `generate_json(&self, prompt, schema) -> Result<serde_json::Value>` — NON-generic, so the trait stays dyn-compatible for `Arc<dyn LlmProvider>` consumers (a generic `<T: DeserializeOwned>` trait method is rustc E0038); typed output comes from the free helper `generate_json_typed<T: DeserializeOwned>(provider: &dyn LlmProvider, ...)` (D-07: provider encapsulates `response_format` request, tolerant `{...}` extraction (D-05), and the 1-shot repair prompt (D-06); workers never see raw strings). Non-streaming (D-08). Also hosts the factory `fn provider_from_config(&LlmSection) -> Arc<dyn LlmProvider>` selecting `openai-compatible | mock` (D-02), and — if the planner puts it here per ARCHITECTURE.md line 104 — the concurrency semaphore.
- **Closest analog (in-repo):** NONE for the trait itself — the repo contains **zero traits today** (grep: no `trait ` in `crates/`). The module *shape* mirrors `store/mod.rs:1-31` (submodule decls + `pub use` re-exports + a struct holding shared state) and `vault/mod.rs:8-14,45-63` (module decls + `Arc`-shared struct, "NOT Clone on purpose" doc discipline).
- **Closest analog (outside repo, reference only):** ARCHITECTURE.md Pattern 2 (`.planning/research/ARCHITECTURE.md:155-163`) already sketches the exact idiom to mirror:
  ```rust
  #[async_trait]
  pub trait SourceConnector: Send + Sync {
      async fn poll(&self, state: &SyncState) -> Result<PollOutcome>;
  }
  ```
  Transpose to: `#[async_trait] pub trait LlmProvider: Send + Sync { async fn complete(&self, prompt: &str) -> Result<String>; async fn generate_json(&self, prompt: &str, schema: &serde_json::Value) -> Result<serde_json::Value> { /* default: D-05/D-06 contract */ } }` returning `crate::error::Result` (error.rs:36 alias — never `anyhow` in the lib). NO generic type parameter on any trait method — that breaks dyn-compatibility (E0038); the typed surface is the free fn `pub async fn generate_json_typed<T: DeserializeOwned>(provider: &dyn LlmProvider, prompt: &str, schema: &serde_json::Value) -> Result<T>`.
- **⚠ NO ANALOG — `async_trait`:** not in workspace deps (`Cargo.toml:13-35`) nor `saussunet-core/Cargo.toml`. Planner adds `async-trait = "0.1"` to `[workspace.dependencies]` + core crate, or uses native `async fn` in traits if the pinned edition (2021, `Cargo.toml:6`) is acceptable — RPITIT cannot be made dyn-compatible without boxing, so `async-trait` is the low-risk choice.
- **Data flow:** `main.rs` boot → `provider_from_config(&cfg.llm)` → `Arc<dyn LlmProvider>` injected into worker pool; tests construct `MockProvider` directly, never via config (D-14).

### A2. `crates/saussunet-core/src/llm/openai_compat.rs` — OpenAI-compatible HTTP impl

- **Role:** `OpenAiCompat { base_url, model, api_key, client: reqwest::Client, timeout }` → `POST {base_url}/chat/completions` with `response_format: {"type":"json_object"}` when supported (D-05 request side), bearer auth, non-streaming (D-08). Tolerant parse + repair (D-05/D-06) shared with the trait-level contract. **The api_key must never appear in logs or error Display strings (D-03)** — error variants carry the URL and status only.
- **⚠ NO ANALOG — outbound HTTP client.** The repo has zero HTTP client code (core is deliberately server-free, `lib.rs:5-7`; reqwest is not a dep, `crates/saussunet-core/Cargo.toml:12-24`). The planner establishes this pattern fresh:
  - Add to `[workspace.dependencies]` (root `Cargo.toml:13` block): `reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }` (rustls keeps the OpenSSL-free build MoriNet-embeddable, QUAL-05 constraint documented at `Cargo.toml:15-18`).
  - Per CONTEXT.md:88, an outbound client in core is explicitly allowed (it is not an HTTP *server* dep) — but `scripts/check.sh` guards Anti-Pattern 1; planner must confirm the guard's allowlist covers `reqwest` or the CI check will fail.
  - Retry/backoff helper (planner discretion, CONTEXT.md:42): `backon` crate or hand-rolled loop — **NO ANALOG either way**; nothing in the repo retries anything.
- **Data flow:** worker → `generate_json` → HTTP POST → raw string → extract `{...}` block → `serde_json::from_str` → on parse failure, one repair prompt re-using the same client → typed `T` or `CoreError::Llm`.

### A3. `crates/saussunet-core/src/llm/mock.rs` — `MockProvider` (scripted response queue)

- **Role:** Deterministic provider fed a programmed queue of scripted responses in code, including failure scripts: invalid JSON (repair test), HTTP-5xx-equivalent error (retry/backoff test), artificial hang > 5 min expiry (heartbeat/reclaim test) — D-13/D-15. Exported publicly for Phase 3 test use (CONTEXT.md:94).
- **⚠ NO ANALOG — mock provider.** No mocks, no test doubles, no `mockall`/trait-object fixtures anywhere in the repo. Established fresh; suggested shape:
  ```rust
  pub struct MockProvider { script: tokio::sync::Mutex<VecDeque<Scripted>> }
  pub enum Scripted { Ok(serde_json::Value), InvalidJson(String), Fail(String), Hang(Duration) }
  ```
  `Scripted::Fail` returns `CoreError::LlmHttp { status: 500 }` so the retry path is exercised identically to the real client. `tokio::sync::Mutex` usage mirrors `vault/mod.rs:52` (`write_lock: tokio::sync::Mutex<()>` — the repo's only interior-mutability precedent).
- **Data flow:** test constructs `MockProvider::new(vec![...])` → passed as `Arc<dyn LlmProvider>` to worker pool → script consumed in order; `Hang` variant awaits a `tokio::time::sleep` longer than the claim expiry so the reclaim test can kill the worker task mid-call.

### A4. `crates/saussunet-core/src/queue/mod.rs` (or `worker/`) — bounded worker pool

- **Role:** N = `[llm].max_concurrency` tokio tasks (default 1, D-04 — never unbounded fan-out, LLMP-02). Each loop: reap expired claims → `claim_jobs(vault_id, 1)` → execute provider call **with a concurrent heartbeat task refreshing `claimed_at` every 60 s** (D-11) → `complete_job` / `fail_job` (retry with backoff 30 s/2 min/10 min, then `status='dead'`, D-09/D-10). Bounded `tokio::sync::mpsc` "work available" signal per ARCHITECTURE.md Pattern 1 (line 136), with polling fallback so a restart picks up pending rows.
- **Closest analog (in-repo, structural only):** the single-writer discipline of `vault/writer.rs:77-83` — one serialized entry point holding a lock for the whole critical section:
  ```rust
  pub async fn write_page(vault: &Vault, store: &Store, ...) -> Result<WriteOutcome> {
      let _guard = vault.write_lock.lock().await;
  ```
  The worker pool mirrors the *shape* (async fn taking `&Store` + `Arc<dyn LlmProvider>`, long-lived critical section) but not the mechanism — the pool is N independent claimers, not a mutex.
- **⚠ NO ANALOG — `tokio::spawn` loop, interval timers, bounded pool.** Grep confirms zero `tokio::spawn`, `tokio::time::interval`, `interval(`, `CancellationToken` in `crates/` (only three plain `loop {` in vault/mod.rs:173, frontmatter.rs:64, lib.rs:85 — synchronous parse loops, not async tasks). The planner establishes fresh:
  - Workspace `tokio` features (`Cargo.toml:14`) are `["macros","rt-multi-thread","fs","sync"]` — **`"time"` is missing**; required for `sleep`/`interval` (backoff, heartbeat, hang tests). Must be added.
  - Pool bound: `for _ in 0..max_concurrency { tokio::spawn(worker_loop(...)) }` or semaphore — planner discretion (CONTEXT.md:43); either way the bound comes from config, never unbounded.
  - Graceful shutdown: no `CancellationToken` dep exists (`tokio-util` absent); planner decides (join handles vs new dep).
- **Data flow:** `main.rs::boot()` → `spawn_workers(store, provider, &rt.llm)` → per-task: claim → heartbeat spawn → `provider.generate_json(job.payload_json…)` → complete/fail. Backoff waits are in-process sleeps; durability comes from the DB row, not the task.

### A5. `crates/saussunet-core/tests/queue.rs` — durable-queue integration tests

- **Role:** Proves SC3: atomic claim under N concurrent workers (D-12 "tested with N concurrent workers"), retry backoff state transitions, dead-letter after 3 failures with `error` retained (D-09/D-10), and kill-mid-job reclaim after 5-min expiry with heartbeat keeping live jobs alive (D-11).
- **Closest analog:** `crates/saussunet-core/tests/writer.rs:13-32` — the temp-vault fixture harness to copy verbatim in shape:
  ```rust
  async fn setup() -> (TempDir, Arc<Vault>, Arc<Store>) {
      let tmp = TempDir::new().expect("tempdir");
      let rt = rt_for(tmp.path(), "main");
      let (store, vault) = boot_open(&rt).await.expect("boot");
      (tmp, Arc::new(vault), Arc::new(store))
  }
  ```
  plus `tests/init_vault.rs:13-20` (`rt_for` TOML→`FileConfig`→`resolve` idiom) and `Store::connect_memory()` (`store/mod.rs:73-84`) for pure-DB tests — note its single-shared-DB caveat (`store/mod.rs:66-72`) when testing concurrent claims; prefer a file-backed TempDir DB for the concurrency test. `#[tokio::test]` + `tempfile` dev-dep already wired (`saussunet-core/Cargo.toml:26-27`).
- **Test-clock discipline:** expiry/backoff tests must not sleep 5 real minutes — use `tokio::time::pause()`/`start_paused` (tokio `time` feature again) or inject the expiry duration as a parameter into the store methods. Planner picks; flag: `tokio::time::pause` requires the test to be single-threaded current-thread runtime (`#[tokio::test(start_paused = true)]`).

### A6. `crates/saussunet-core/tests/llm_provider.rs` — provider + mock tests

- **Role:** Mock script consumption order, repair-prompt path (invalid JSON → repaired on 2nd scripted response, D-06 repair does NOT consume a queue retry), tolerant `{...}` extraction from noisy prose (D-05), and D-14 proof: the whole pipeline runs with `MockProvider` and zero config reads.
- **Closest analog:** same fixture harness as A5 (`tests/writer.rs:27-32`). No other analog.
- **⚠ NO ANALOG — real-endpoint integration test (D-01 cloud-first validation).** No env-gated/`#[ignore]` test precedent exists. Planner establishes: `#[ignore = "requires SAUSSUNET_LLM__API_KEY"]` test hitting a configured endpoint, CI never runs it (D-14: "CI has no API keys").

---

## B. Modified files

### B1. `crates/saussunet-core/src/store/jobs.rs` — claim / complete / fail / reap / heartbeat SQL

- **Role:** Add `claim_jobs(vault_id, n) -> Vec<Job>` (atomic, single transaction, D-12), `touch_claim(job_id)` (heartbeat refresh), `complete_job(job_id)`, `fail_job(job_id, error, max_attempts, backoffs)` (attempts++ → `pending` with `next_attempt_at` gate, or `dead` past 3 retries), `reap_expired_claims(vault_id, expiry)` (claimed → pending).
- **Closest analog (same file):** `jobs.rs:26-34` `insert_job` (bind-chain + `execute(&self.pool)` idiom) and `jobs.rs:40-64` `list_jobs` (explicit column list, claim-order `ORDER BY priority DESC, created_at, id` — the claim query must use the same order to match `idx_jobs_claim`, migration `0003_jobs_sync_state.sql:19`). The file's own header (`jobs.rs:1-2`) and `list_jobs` doc (`jobs.rs:38-39`) explicitly reserve this work for Phase 2.
- **Closest analog (transaction shape):** `reindex.rs:96-113` — the repo's ONLY sqlx transaction precedent:
  ```rust
  let mut tx = store.pool().begin().await?;
  Store::clear_pages_tx(&mut *tx, vault.id).await?;
  ...
  tx.commit().await?;
  ```
  Mirror the `*_tx(&mut *tx, ...)` free-function pattern from `store/pages.rs:24-25,63` ("on any executor (pool OR transaction)") for the claim statement.
- **Claim SQL to mirror (D-12 verbatim + ARCHITECTURE.md:141-147):**
  ```sql
  UPDATE jobs SET status='claimed', claimed_at=datetime('now')
  WHERE id IN (SELECT id FROM jobs WHERE status='pending' AND vault_id=?
               ORDER BY priority DESC, created_at LIMIT ?)
  RETURNING id, vault_id, kind, payload_json, ...
  ```
  Run inside `pool().begin()` (D-12 says single transaction; SQLite serializes writers + `busy_timeout(5s)` at `store/mod.rs:49` handles contention). Note ARCHITECTURE.md uses `status='running'`; D-10/D-12 use `'claimed'` — **D-12 wins**, and `status` has no CHECK constraint (`0003_jobs_sync_state.sql:9`), so **no new migration is needed** for new status values (schema frozen per Phase 1 D-02). Retry backoff needs a due-time gate: either reuse `claimed_at` as "not before" or add a `next_attempt_at` column — a column addition would violate the frozen-schema note, so prefer gating in SQL on existing columns or document the migration exception; planner decides explicitly.
- **Data flow:** worker pool → these `&self` async methods on shared `Store` (pattern: `store/mod.rs:29-31` + CONTEXT.md:89 "queue claim logic belongs in store/jobs.rs as SQL transactions").

### B2. `crates/saussunet-core/src/config.rs` — `[llm]` section + env override

- **Role:** Add `LlmSection { provider, base_url, model, api_key, max_concurrency (default 1), timeout }` to `FileConfig` (D-02/D-03/D-04), resolved into `RuntimeConfig` with `SAUSSUNET_LLM__API_KEY` env override applied (D-03).
- **Closest analog (section shape):** `config.rs:40-46 + 64-71 + 81-87` — the `VaultSection` triad exactly: `#[serde(default)]` field on `FileConfig` (line 17-18), struct with `#[serde(default = "default_x")]` per field, and a **manual `Default` impl** (comment at `config.rs:29-31`: derived Default would bypass the serde default fns). Add `#[serde(default)] pub llm: LlmSection` at `config.rs:18` and mirror the section-default tests at `config.rs:138-146` ("bare `[llm]` must parse").
- **Closest analog (env override):** MoriNet `/home/c/Projects/Dev/MoriNet/src/config.rs:141-157` (reference only, not a dep):
  ```rust
  let admin_password = std::env::var(ENV_ADMIN_PASSWORD)
      .ok()
      .or_else(|| cfg.admin.password.clone().filter(|s| !s.is_empty()))
      .ok_or_else(...)?;
  ```
  Transpose as `env("SAUSSUNET_LLM__API_KEY").ok().or(cfg.llm.api_key)` inside `FileConfig::resolve()` (`config.rs:103-112`) — env wins over file (D-03). MoriNet's env-test discipline (`config.rs:179-181`: serialize env-mutating tests behind a static Mutex because cargo shares process env) must be copied into the new tests.
- **D-03 secrecy:** keep `api_key` out of `Debug` output — `#[derive(Debug)]` on `RuntimeConfig` (`config.rs:90`) would print it; use a `Debug`-redacting wrapper or manual impl. Flag for planner.

### B3. `crates/saussunet-core/src/error.rs` — LLM/queue error variants

- **Role:** Extend `CoreError` with e.g. `Llm { ... }` (HTTP status + message, **never the api_key**, D-03), `LlmParse(String)` (unparseable JSON after repair, D-06), `Queue(String)` (claim/transition invariant breaks).
- **Closest analog (same file):** `error.rs:10-33` — thiserror enum with string-payload variants (`Config(String)`, `Vault(String)`) and structured variants (`Io { path, #[source] source }`). Mirror `Io`'s field style for `Llm { status: u16, url: String, body: Option<String> }`; mirror `Store(#[from] sqlx::Error)` (line 15-16) if a reqwest `#[from]` conversion is wanted (planner may prefer explicit constructors to control what leaks into Display).
- **Note:** `error.rs:1-4` itself declares "no MoriNet analog; established fresh" — this file IS the repo's pattern authority; just extend it, keep `Result<T>` alias (line 36) as the only error surface.

### B4. `crates/saussunet-core/src/lib.rs` — module wiring + `default_config()` update

- **Role:** `pub mod llm;` + `pub mod queue;` (module list at `lib.rs:9-13`); extend `default_config()` template string with the `[llm]` section; possibly a `spawn_workers` re-export used by the binary.
- **Closest analog:** `lib.rs:130-147` `default_config()` (raw-string TOML template) — and its hard coupling: `lib.rs:161-170` asserts the committed root `config.toml` is **byte-identical** to this string, so B5 must change in lockstep. Boot wiring shape: `boot_open` (`lib.rs:33-58`) returns `(Store, Vault)`; Phase 2 adds provider construction + pool spawn after it in the binary rather than inside `boot_open` (keeps boot testable without a provider) — planner decision, both shapes have precedent.
- **Data flow:** config section → `provider_from_config` → `Arc<dyn LlmProvider>` → `queue::spawn_workers(store, provider, max_concurrency)`.

### B5. `config.toml` (workspace root) — add `[llm]` block

- **Role:** Ship the new section in the committed template (mock provider default so a fresh checkout runs with zero secrets — D-14 spirit).
- **Closest analog:** the file itself + `lib.rs:131-146` (the two are test-pinned byte-identical; edit both in the same commit or `committed_root_config_matches_default_config` fails).

### B6. `crates/saussunet/src/main.rs` — wire provider + worker pool into boot

- **Role:** In `boot()` (`main.rs:117-129`), after `saussunet_core::boot_open(&rt)`, construct the provider from `cfg.llm` and spawn the bounded pool; bare run keeps running the pool (currently boot prints "vault ready" and exits — Phase 2 must decide whether bare run blocks on workers; ROADMAP SC1/SC4 implies the daemon now stays alive).
- **Closest analog:** `main.rs:117-129` boot sequence (config → resolve → boot_open → print) and the runtime builder at `main.rs:86-89` (`Builder::new_multi_thread().enable_all()` — `enable_all` already enables time, so the binary is fine; the *lib's* dev/test tokio features are the gap, B7/A4).
- **Data flow:** `load_config_or_default` → `cfg.resolve()` → `boot_open` → provider factory → `spawn_workers` → block forever (e.g. `tokio::signal` or join on handles — **NO ANALOG** for long-lived daemon mode; every current command exits).

### B7. `Cargo.toml` (workspace root) + `crates/saussunet-core/Cargo.toml` — new deps

- **Role:** Add `reqwest`, `async-trait`, and (if chosen) `backon` to `[workspace.dependencies]` (root `Cargo.toml:13-35`, single-sourced-version policy documented at lines 10-12) and reference them `{ workspace = true }` in `saussunet-core/Cargo.toml:12-24`. **Extend tokio features with `"time"`** (`Cargo.toml:14`) — needed by heartbeat, backoff sleeps, and paused-time tests.
- **Closest analog:** the existing workspace-dep blocks themselves; comment style for non-obvious pins (`Cargo.toml:15-18, 31-32`) should be copied for the reqwest rustls rationale.
- **Check `scripts/check.sh`:** it guards "core has zero HTTP deps" (lib.rs:5-7); planner must verify the guard distinguishes axum/server deps from the reqwest client, or update the guard with rationale.

---

## C. NO ANALOG gaps (planner must establish fresh)

1. **Outbound HTTP client** — no reqwest/HTTP client anywhere in the repo; request/response/timeout/error-mapping pattern is new (A2).
2. **Trait-based provider abstraction** — repo has zero traits; `async_trait` dep + dyn-injection pattern new, only sketched in ARCHITECTURE.md Pattern 2 (A1).
3. **MockProvider scripted response queue** — no test doubles exist anywhere (A3).
4. **Async worker pool / `tokio::spawn` loops** — zero spawns, zero interval timers, zero CancellationToken in the repo; only synchronous `loop {}` in parsers (A4).
5. **Heartbeat (`claimed_at` refresh every 60 s)** — nothing periodic exists; also blocked on tokio `"time"` feature gap (A4, B7).
6. **Retry/backoff helper** — nothing retries anything; backon-vs-hand-rolled is an open planner decision (CONTEXT.md:42).
7. **Env-var secret override in-repo** — analog exists only in MoriNet (external reference, `MoriNet/src/config.rs:141-157`), including its env-test serialization discipline (B2).
8. **Env-gated real-endpoint integration test** (D-01 cloud-first proof) — no `#[ignore]`/env-gated test precedent (A6).
9. **Long-lived daemon mode in the binary** — every current subcommand exits; blocking-forever wiring has no precedent (B6).
10. **Retry due-time gating without a schema change** — frozen jobs schema has no `next_attempt_at`; SQL-only workaround or documented migration exception (B1).

---

## D. File inventory summary

| # | File | New/Modified | Analog quality |
|---|------|--------------|----------------|
| 1 | `crates/saussunet-core/src/llm/mod.rs` | new | NO ANALOG (trait) / store-mod shape |
| 2 | `crates/saussunet-core/src/llm/openai_compat.rs` | new | NO ANALOG |
| 3 | `crates/saussunet-core/src/llm/mock.rs` | new | NO ANALOG |
| 4 | `crates/saussunet-core/src/queue/mod.rs` (worker pool) | new | weak analog (writer lock shape) |
| 5 | `crates/saussunet-core/src/store/jobs.rs` | modified | strong (same file + reindex tx) |
| 6 | `crates/saussunet-core/src/config.rs` | modified | strong (VaultSection + MoriNet env) |
| 7 | `crates/saussunet-core/src/error.rs` | modified | strong (same file) |
| 8 | `crates/saussunet-core/src/lib.rs` | modified | strong (default_config coupling) |
| 9 | `config.toml` (root) | modified | strong (byte-identical test) |
| 10 | `crates/saussunet/src/main.rs` | modified | strong (boot path) / NO ANALOG (daemon mode) |
| 11 | `Cargo.toml` (workspace root) | modified | strong (dep blocks) |
| 12 | `crates/saussunet-core/Cargo.toml` | modified | strong |
| 13 | `crates/saussunet-core/tests/queue.rs` | new | strong (writer.rs fixture) |
| 14 | `crates/saussunet-core/tests/llm_provider.rs` | new | strong (fixture) / NO ANALOG (mock, real-endpoint test) |

*Module names in A1–A4 follow ARCHITECTURE.md's `llm/` layout; CONTEXT.md:44 leaves final layout to the planner — remap paths if the planner chooses `worker/` or flat modules.*

*Pattern mapping for: Phase 2 — LLM Provider + Durable Job Queue*
