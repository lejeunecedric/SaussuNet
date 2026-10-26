---
phase: 02-llm-provider-durable-job-queue
plan: 01
subsystem: config, error-taxonomy, workspace-deps
tags: [rust, reqwest, rustls, async-trait, tokio, thiserror, serde, toml, secret-redaction]

requires:
  - phase: 01-workspace-foundations-vault-writer
    provides: "FileConfig/RuntimeConfig two-stage config, CoreError taxonomy, byte-identical default_config() coupling, check.sh quality gate"
provides:
  - "[llm] config section (LlmSection) with D-02/D-03/D-04 defaults end-to-end"
  - "SecretString newtype redacting Debug to SecretString(\"***\")"
  - "SAUSSUNET_LLM__API_KEY env override in resolve() (env wins over file)"
  - "CoreError::Llm { status, url } / LlmParse(String) / Queue(String)"
  - "Workspace deps reqwest 0.12 (rustls) + async-trait 0.1; tokio features +time +signal"
affects: [02-02, 02-03, 02-04, llm-provider, job-queue, worker-pool]

tech-stack:
  added: [reqwest 0.12 (default-features=false, json, rustls-tls), async-trait 0.1]
  patterns:
    - "SecretString newtype: Deserialize via String, reveal() accessor, manual Debug redaction"
    - "Section triad: serde struct + per-field #[serde(default = \"...\")] + manual Default impl"
    - "ENV_LOCK static Mutex serializing env-mutating tests (MoriNet discipline)"

key-files:
  created: []
  modified:
    - Cargo.toml
    - crates/saussunet-core/Cargo.toml
    - crates/saussunet-core/src/error.rs
    - crates/saussunet-core/src/config.rs
    - crates/saussunet-core/src/lib.rs
    - config.toml
    - crates/saussunet-core/tests/init_vault.rs

key-decisions:
  - "HTTP client = reqwest 0.12 (planner decision, PATTERNS A2): default-features=false + rustls-tls keeps the OpenSSL-free MoriNet-embeddable build (QUAL-05)"
  - "Backoff = hand-rolled fixed schedule 30s/2min/10min, NO backon dependency (D-09's three fixed delays don't justify a new crate)"
  - "Trait plumbing = async-trait 0.1 (RPITIT is not dyn-compatible without boxing on edition 2021)"
  - "CoreError::Llm constructed explicitly — intentionally NO #[from] reqwest::Error, so nothing can leak via Display (D-03)"

requirements-completed: [LLMP-01]

duration: 6min
completed: 2026-10-26
---

# Phase 2 Plan 01: [llm] Config Foundation Summary

**`[llm]` TOML section resolving end-to-end — SecretString-redacted api_key with SAUSSUNET_LLM__API_KEY env override (env wins), D-02/D-04 defaults (mock provider, max_concurrency=1, timeout 300s), plus reqwest/async-trait workspace deps and Llm/LlmParse/Queue error variants.**

## Performance

- **Duration:** 6 min
- **Started:** 2026-10-26T07:25:24Z
- **Completed:** 2026-10-26T07:31:21Z
- **Tasks:** 2
- **Files modified:** 7

## Accomplishments
- `FileConfig::resolve()` now produces `llm_provider/llm_base_url/llm_model/llm_api_key/llm_max_concurrency/llm_timeout_secs`; a bare `[llm]` parses with provider="mock", max_concurrency=1, timeout_secs=300 (D-02, D-04).
- D-03 secrecy: `SecretString` newtype renders `SecretString("***")` in Debug; test `debug_output_never_leaks_api_key` asserts the key literal is absent from `format!("{rt:?}")`; `CoreError::Llm` carries only `{status, url}`.
- Env override `SAUSSUNET_LLM__API_KEY` beats the file value at resolve() time; empty env var is ignored (file value survives). Env-mutating tests serialized behind `static ENV_LOCK: Mutex<()>`.
- Workspace deps landed so later Phase 2 plans add no Cargo churn: reqwest 0.12.28 (rustls), async-trait 0.1, tokio features extended with "time" + "signal".
- Root `config.toml` and `lib.rs::default_config()` edited in lockstep — `committed_root_config_matches_default_config` green; committed template ships `provider = "mock"` (zero-secrets fresh checkout).

## Task Commits

1. **Task 1: Workspace deps + LLM/queue error variants** — `4f98dd6` (feat)
2. **Task 2: [llm] config section with env-override secret + Debug redaction** — `883962f` (feat)

**Plan metadata:** this SUMMARY commit (docs)

## Files Created/Modified
- `Cargo.toml` — +reqwest 0.12 (json, rustls-tls, rationale comment), +async-trait 0.1; tokio features +"time","signal"
- `crates/saussunet-core/Cargo.toml` — reqwest + async-trait `{ workspace = true }` (no axum — guard intact)
- `crates/saussunet-core/src/error.rs` — `Llm { status: u16, url: String }`, `LlmParse(String)`, `Queue(String)`; no reqwest From impl
- `crates/saussunet-core/src/config.rs` — `SecretString`, `LlmSection` (triad pattern), llm fields on FileConfig/RuntimeConfig, env override in resolve(), 5 new unit tests
- `crates/saussunet-core/src/lib.rs` — default_config() gains the `[llm]` block
- `config.toml` — byte-identical `[llm]` block (mock default, commented examples)
- `crates/saussunet-core/tests/init_vault.rs` — RuntimeConfig literal extended with the new llm fields (compile fix)

## Decisions Made
- The three planner decisions from the plan were recorded verbatim in frontmatter key-decisions (reqwest 0.12 rustls / hand-rolled backoff, no backon / async-trait 0.1).
- `SecretString` exposes `reveal()` as the only sanctioned read path, keeping redaction the default everywhere else.

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug/Compile] `tests/init_vault.rs` RuntimeConfig literal needed the new llm fields**
- **Found during:** Task 2 (`cargo test` failed E0063: missing fields in the hand-built RuntimeConfig at tests/init_vault.rs:233)
- **Issue:** The plan's files_modified did not list `tests/init_vault.rs`, but it constructs `RuntimeConfig` directly and broke when the llm fields were added.
- **Fix:** Added the six llm fields with mock defaults to the literal.
- **Files modified:** crates/saussunet-core/tests/init_vault.rs
- **Verification:** `cargo test -p saussunet-core` fully green.
- **Committed in:** 883962f (Task 2 commit)

**2. [Process] `scripts/check.sh` run once at plan end instead of after Task 1**
- Task 1's `<verify>` includes check.sh, but it embeds a full release build; running it twice would duplicate ~minutes of compilation. Task 1 was verified with `cargo build` + `cargo clippy --all-targets -D warnings` + all acceptance greps; the full `bash scripts/check.sh` ran once after Task 2 and passed (`check: OK`), satisfying the plan-level `<verification>` identically.

---

**Total deviations:** 2 (1 auto-fixed compile break, 1 process consolidation)
**Impact on plan:** The init_vault.rs fix was mandatory for compilation; no scope creep. All acceptance criteria and plan-level verification pass.

## Issues Encountered
None beyond the deviations above. (One tool-level hiccup: a patch edit briefly dropped a `]` in error.rs and was caught and fixed before the Task 1 commit — no trace in committed code.)

## Verification Evidence
- `cargo test -p saussunet-core`: 57 lib + 8 + 6 + 1 + 7 + 9 integration = **88 passed, 0 failed** (includes `committed_root_config_matches_default_config`, `bare_llm_section_parses_with_defaults`, `env_api_key_overrides_file_value`, `debug_output_never_leaks_api_key`)
- `bash scripts/check.sh`: fmt, clippy -D warnings (all targets), tests, axum-absence guard, release build — **check: OK**
- `cargo tree -p saussunet-core | grep -c axum` → 0; reqwest resolves to 0.12.28
- `grep -c 'SAUSSUNET_LLM__API_KEY' crates/saussunet-core/src/config.rs` → 3

## Next Phase Readiness
- Plans 02-02..02-04 can build `llm/` (trait + mock + openai_compat) and `queue/` directly on these deps and error variants — no Cargo churn expected.
- `LlmSection` is the input for the future `provider_from_config` factory; `max_concurrency` (default 1) is ready to size the worker pool (D-04).

---
*Phase: 02-llm-provider-durable-job-queue*
*Completed: 2026-10-26*
