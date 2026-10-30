---
phase: 03-extraction-pipeline
plan: 02
subsystem: http-api
one_liner: "POST {base}/ingest now validates an SSRF-guarded URL, inserts a durable ingest_url job and returns its job id under a configurable base path — axum lives only in the binary crate."
tags: [axum, axum-router, tower, axum-test, ssrf, config, base-path]

requires:
  - phase: 03-extraction-pipeline/03-01
    provides: [JOB_KIND_INGEST_URL, IngestUrlPayload, pipeline::fetch::assert_public_http_url, store.insert_job]
  - phase: 02-llm-provider-durable-job-queue
    provides: [spawn_workers, boot_open, MockProvider]
provides:
  - "saussunet::http::build_router(AppState) — GET {base}/health + POST {base}/ingest, nested under the configured base path"
  - "RuntimeConfig.base_path + pipeline_min_sources / pipeline_max_upload_bytes / pipeline_max_fetch_bytes"
  - "[server].base_path and [pipeline] keys in default_config() ↔ config.example.toml"
  - "main::boot third long-lived task: axum::serve on rt.bind, aborted on ctrl_c"
affects: [03-03, 03-05, 03-06, phase-5-ui]

tech-stack:
  added: [axum 0.8 (binary crate only), tower 0.5 util (dev), http-body-util 0.1 (dev)]
  patterns:
    - "router-level tests via tower::ServiceExt::oneshot against a temp-vault Store (no socket) + one real-bind smoke test"
    - "config section triad: #[serde(default)] on FileConfig + #[serde(default = \"…\")] per field + manual Default impl"

key-files:
  created:
    - crates/saussunet/src/http.rs
    - crates/saussunet/src/lib.rs
    - crates/saussunet/tests/http_ingest.rs
  modified:
    - Cargo.toml
    - crates/saussunet/Cargo.toml
    - crates/saussunet/src/main.rs
    - crates/saussunet-core/src/config.rs
    - crates/saussunet-core/src/lib.rs
    - crates/saussunet-core/tests/init_vault.rs
    - config.example.toml

key-decisions:
  - "axum 0.8 added to [workspace.dependencies] but referenced only by crates/saussunet; core's Cargo.toml untouched so the check.sh axum-absence guard stays meaningful"
  - "base_path normalized in FileConfig::resolve() (leading slash, no trailing slash, empty/blank → default) so axum::Router::nest can never panic on a malformed prefix"
  - "All Phase 3 config plumbing lands here (base_path + [pipeline]) so 03-03 does not touch config files"
  - "Submit-time assert_public_http_url is defense in depth only; the authoritative SSRF guard stays in the worker fetch path"

patterns-established:
  - "HTTP surface performs zero vault writes — its only side effect is store.insert_job"
  - "Request logging carries method, path, status and job id only; bodies are never logged"

requirements-completed: [PIPE-01]

duration: ~35min
completed: 2026-10-30
---

# Phase 3 Plan 02: POST /ingest HTTP entry point Summary

**`POST {base}/ingest` accepts a URL, rejects blocked or malformed submissions with 4xx without writing a job row, and otherwise inserts a durable `ingest_url` job and answers `202 {"job_id": N}` — all routes nested under the configurable `/saussunet` base path, with axum confined to the binary crate.**

## Performance

- **Duration:** ~35 min
- **Completed:** 2026-10-30T12:06Z
- **Tasks:** 2 (both TDD: RED observed before each implementation, then GREEN)
- **Files created/modified:** 10

## What Was Built

### Task 1 — config: `[server].base_path` + `[pipeline]` section (`877510e`)
- `config.rs`: `ServerSection.base_path` (default `/saussunet`) and a new `PipelineSection { min_sources = 2 (D-14), max_upload_bytes = 26_214_400, max_fetch_bytes = 5_242_880 }`, both following the existing `VaultSection` triad pattern (`#[serde(default)]` on `FileConfig` + per-field `#[serde(default = "…")]` + manual `Default` impl), so every section parses when absent.
- `normalize_base_path(&str)` applied in `FileConfig::resolve()`: trims whitespace, forces a leading slash, strips trailing slashes, and falls back to the default when the result would be empty (`""`, `"saussunet"`, `"/"`, `"/x/"` → `/saussunet`, `/saussunet`, `/saussunet`, `/x`). Stored on `RuntimeConfig` as `base_path`, alongside `pipeline_min_sources`, `pipeline_max_upload_bytes`, `pipeline_max_fetch_bytes`.
- `lib.rs::default_config()` and root `config.example.toml` updated in the same commit (byte-identical test), `[pipeline]` block carrying a comment naming D-14.

### Task 2 — axum router + boot wiring + tests (`cbdd3cf`)
- `crates/saussunet/src/http.rs` — the only axum code in the repo. `AppState { store: Arc<Store>, vault_id, base_path }`; `build_router` defines `GET /health` → 200 `"ok"` and `POST /ingest` then `.nest(&base_path, app)` (D-13) — any other prefix 404s, no root fallback.
- `ingest` handler: `DefaultBodyLimit::max(65_536)` on the route → `serde_json` parse of `IngestReq { url }` (400 on bad JSON, missing field or blank url) → `pipeline::fetch::assert_public_http_url(&url)` (400 with the guard's pre-sanitized reason) → `insert_job(vault_id, JOB_KIND_INGEST_URL, {"url","source_id"})` → `202 {"job_id": N}`. Zero vault writes, zero LLM contact, bodies never logged (method/path/status/job_id only).
- `main.rs::boot()`: `mod http` exposed via a new `crates/saussunet/src/lib.rs` (needed so the integration test can reach `build_router`), `TcpListener::bind(&rt.bind)` + `tokio::spawn(axum::serve(...))` as the third long-lived task after `spawn_workers`, prints `http ready: http://{bind}{base_path}/ingest`, and `ctrl_c` aborts the serve task plus the worker pool. `daemon_drains_queue` mock-provider behaviour untouched.
- Workspace `axum = "0.8"` with a comment recording the binary-only rationale; `crates/saussunet` gained `axum` plus dev-deps `tower` (util), `http-body-util`, `reqwest`, `tempfile`, `toml`. Core's `Cargo.toml` untouched.

## Test Names Added

- `config::tests::bare_server_section_gets_base_path_default`
- `config::tests::bare_pipeline_section_parses_with_defaults`
- `config::tests::absent_pipeline_section_gets_sane_defaults`
- `config::tests::full_pipeline_section_resolves`
- `config::tests::base_path_normalization`
- `tests/http_ingest.rs::post_ingest_valid_url_returns_202_and_inserts_pending_job`
- `tests/http_ingest.rs::post_ingest_without_base_path_is_404`
- `tests/http_ingest.rs::post_ingest_blocked_url_is_400_and_inserts_no_job`
- `tests/http_ingest.rs::post_ingest_malformed_body_is_400`
- `tests/http_ingest.rs::health_under_base_path_is_200_ok`
- `tests/http_ingest.rs::real_bind_serves_health_over_loopback`

TDD evidence — RED before Task 1: `cargo test -p saussunet-core config` → `error[E0609]: no field 'base_path' on type 'RuntimeConfig'` (13 errors). RED before Task 2: `cargo test -p saussunet --test http_ingest` → `unresolved module or unlinked crate 'toml'` / `cannot find module or crate 'tracing'`.

Final results: `cargo test -p saussunet --test http_ingest` → `test result: ok. 6 passed; 0 failed`; `cargo test -p saussunet-core config` → `test result: ok. 21 passed; 0 failed` (includes `committed_example_config_matches_default_config`); core lib `87 passed; 0 failed`; every Phase 1/2/3 suite green; `bash scripts/check.sh` → `check: OK` (fmt, clippy `-D warnings`, zero-real-LLM guard, axum-absence guard, release build).

Acceptance greps: `grep -c axum crates/saussunet-core/Cargo.toml` → `0`; `grep -n 'nest' crates/saussunet/src/http.rs` → line 42 `Router::new().nest(&base_path, app)`; `grep -n 'base_path' crates/saussunet/src/main.rs` → lines 142, 175; `grep -n 'base_path' config.rs` → field + `default_base_path` + `normalize_base_path` + resolve call; `grep -n 'min_sources' config.rs` → non-empty.

Manual smoke (`saussunet` run in a tempdir): `curl -s -X POST localhost:8080/saussunet/ingest -H 'content-type: application/json' -d '{"url":"https://example.com"}'` → `{"job_id":1}`; `curl -s localhost:8080/saussunet/health` → `ok`; jobs table row `(1, 'ingest_url', 'pending', '{"source_id":"https://example.com","url":"https://example.com"}')`.

## Task Commits

1. **Task 1: config `[server].base_path` + `[pipeline]` section** — `877510e` (feat + test)
2. **Task 2: axum router, boot wiring, router tests** — `cbdd3cf` (feat + test)

## Files Created/Modified

- `crates/saussunet/src/http.rs` — created: `AppState`, `build_router`, `health`, `ingest`
- `crates/saussunet/src/lib.rs` — created: binary-crate lib target exposing `pub mod http` for integration tests
- `crates/saussunet/tests/http_ingest.rs` — created: 6 router/bind tests
- `crates/saussunet/src/main.rs` — `mod http` wiring, serve task, `http ready` line, ctrl_c abort
- `crates/saussunet/Cargo.toml` — `axum` dep + dev-deps (`tower`, `http-body-util`, `reqwest`, `tempfile`, `toml`)
- `Cargo.toml` — `axum = "0.8"` in `[workspace.dependencies]` with binary-only rationale
- `crates/saussunet-core/src/config.rs` — `base_path`, `PipelineSection`, `normalize_base_path`, RuntimeConfig fields + 5 tests
- `crates/saussunet-core/src/lib.rs` — `default_config()` template with the new keys
- `crates/saussunet-core/tests/init_vault.rs` — `RuntimeConfig` literal extended with the new fields
- `config.example.toml` — `base_path` + `[pipeline]` block (lockstep with `default_config()`)

## Deviations from Plan

**1. [Rule - Forced by plan constraint] `crates/saussunet/src/lib.rs` added (not in `files_modified`)**
- **Found during:** Task 2 (axum router + tests)
- **Issue:** the plan puts `build_router` in the binary crate and tests it from `crates/saussunet/tests/http_ingest.rs`, but integration tests cannot import from a `[[bin]]`-only crate — there was no lib target to import `saussunet::http` from.
- **Fix:** added a minimal `src/lib.rs` containing `pub mod http;`; `main.rs` uses `use saussunet::http;` instead of declaring the module itself. The binary still ships the same single `saussunet` executable.
- **Verification:** `cargo test -p saussunet --test http_ingest` → 6 passed; `cargo build --release` green in `check.sh`.
- **Committed in:** `cbdd3cf`

**2. [Rule - Forced by plan constraint] `crates/saussunet-core/tests/init_vault.rs` extended (not in `files_modified`)**
- **Found during:** Task 1 (config)
- **Issue:** `boot_refuses_db_inside_vault_root` constructs a `RuntimeConfig` literal, which stopped compiling once `base_path` and the three `pipeline_*` fields were added.
- **Fix:** added the four fields with their documented defaults to that literal.
- **Verification:** `cargo test -p saussunet-core` (all suites) green; `check.sh` green.
- **Committed in:** `877510e`

**3. [Rule - Minor] Extra dev-dependencies in `crates/saussunet/Cargo.toml`**
- **Found during:** Task 2 (tests)
- **Issue:** the plan listed `axum`, `tower` and `http-body-util`; the fixture and handler additionally need `toml` (test config fixture parses TOML), `reqwest` (the plan's own real-bind smoke test), `tempfile` (temp vault) and, in the crate's regular deps, `serde` + `tracing` (used by `http.rs`).
- **Fix:** added them, all workspace-sourced except `http-body-util`/`tower` as pinned by the plan.
- **Verification:** clippy `-D warnings` and `check.sh` green.
- **Committed in:** `cbdd3cf`

**Total deviations:** 3 auto-fixed (2 forced by the plan's own file/test layout, 1 dependency completeness).
**Impact on plan:** no scope creep; every acceptance criterion was met as written. Core remains axum-free and the guard is unchanged.

## Decisions

- Normalization lives in `resolve()` (not in the serde default) so a hand-written `base_path = "saussunet"` in a user config is corrected exactly once, and `Router::nest` can never receive a prefix that makes axum panic.
- `AppState` carries only the `RuntimeConfig` subset the ingest route needs (`store`, `vault_id`, `base_path`). `pipeline_min_sources` / `max_*_bytes` are resolved and available but deliberately not consumed yet — `min_sources` is used by 03-03 and `max_upload_bytes` by 03-05.
- The `ingest` handler takes the body as `String` and parses with `serde_json` itself rather than using `axum::Json`, so a malformed body yields a plain-text 400 with the serde reason instead of axum's default rejection shape, and the 64 KiB `DefaultBodyLimit` still applies.
- `source_id` is set to the URL verbatim, matching `IngestUrlPayload`'s documented default from 03-01.
- The blocked-URL test uses `http://169.254.169.254/` (the canonical link-local metadata address) and the valid-URL test uses the public literal `http://93.184.216.34/page`, so no test performs outbound DNS or HTTP.

## Lessons

- A `[[bin]]`-only crate cannot be exercised by integration tests; adding a thin `src/lib.rs` that re-exports the module under test is the smallest fix and keeps the binary's public surface unchanged.
- Adding fields to a `pub struct` in core breaks every out-of-crate struct literal — `grep -rn 'RuntimeConfig {'` across `tests/` before committing a config change avoids a surprise red suite.
- `axum::Router::nest` panics on a prefix without a leading slash or with a trailing one, so normalizing the configured base path at resolve time is a correctness requirement, not cosmetics.
- `tower::ServiceExt::oneshot` consumes the router; a test issuing two requests needs `app.clone()` on the first call (clippy flags the redundant clone if you clone on the last one).

## Issues Encountered

- `gsd-sdk query agent-skills gsd-executor` returned empty on this install (known) — followed `~/.hermes/profiles/devo/get-shit-done/workflows/execute-plan.md` + the summary template, per dispatch instructions.
- `sqlite3` is not installed on this host, so the jobs-table verification from the plan's Verification section was done with Python's stdlib `sqlite3` module instead; the row was confirmed as `(1, 'ingest_url', 'pending', …)`.

## Next Phase Readiness

- 03-03 can read `RuntimeConfig.pipeline_min_sources` and extend `run_extraction_passes`; no config-file changes needed.
- 03-05 adds the upload route to the same `build_router` and already has `pipeline_max_upload_bytes` resolved; remember to keep `DefaultBodyLimit` per-route so the URL route keeps its 64 KiB cap.
- Phase 5 UI extends the same nested router under `base_path` — no retrofit required (D-13 satisfied).

---
*Phase: 03-extraction-pipeline*
*Completed: 2026-10-30*
