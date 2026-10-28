---
phase: 02-llm-provider-durable-job-queue
verified: 2026-10-26T08:36:20Z
status: passed
score: "26/26 must-haves verified"
behavioral_verification:
  test_suite: "cargo test -p saussunet-core → 121 passed, 0 failed, 1 ignored (matches expected)"
requirements_coverage:
  LLMP-01: satisfied
  LLMP-02: satisfied
  SRCC-03: satisfied
decision_coverage:
  honored: 14
  total: 14
  not_honored: []
human_verification:
  - "RESOLVED 2026-10-26: live end-to-end validation against a real OpenAI-compatible endpoint (D-01, SC1 live half) — run against local Halogen endpoint (http://127.0.0.1:8731/v1, model halogen-qwen3.8-flash-next): live_endpoint_returns_valid_json PASSED (1 passed, 0.73s). See 02-HUMAN-UAT.md"
gaps: []
deferred: []
---

# Phase 02 Verification: LLM Provider + Durable Job Queue

**Goal (ROADMAP):** Jobs flow through a crash-safe queue to extraction workers backed by a swappable LLM provider — fully testable with a deterministic mock before any real model is configured.

**Overall status: `passed`** — every automated check passes (26/26 truths, all artifacts wired, 121/121 tests green, all 14 CONTEXT decisions honored). The *live* half of SC1 was resolved 2026-10-26: the env-gated live test ran against the local Halogen OpenAI-compatible endpoint and passed (see human_verification above and 02-HUMAN-UAT.md).

## Success Criteria (ROADMAP)

| # | Criterion | Status | Evidence |
|---|-----------|--------|----------|
| SC1 | Any OpenAI-compatible endpoint configured via `LlmProvider`; a job completes against it | ⚠️ PARTIAL (code ✓, live run pending) | `llm/openai_compat.rs:97` `impl LlmProvider for OpenAiCompat`; `provider_from_config` (`llm/mod.rs:41`) mock/openai-compatible arms; daemon boot in `main.rs:129-147`; `tests/llm_live.rs` env-gated — never executed here (no credentials) |
| SC2 | Deterministic mock drives entire pipeline; CI passes with zero real LLM | ✓ VERIFIED | `MockProvider` + `Scripted` (`llm/mock.rs:26-45`); 9 tests in `tests/llm_provider.rs`, 6 in `tests/queue.rs`; `scripts/check.sh`: grep guard forbids `OpenAiCompat` in mock test files + `env -u SAUSSUNET_LLM__API_KEY cargo test` |
| SC3 | SQLite queue with atomic claim, retry states, claimed-at expiry; kill mid-batch resumes, no loss/duplication | ✓ VERIFIED | `claim_jobs` single-transaction `UPDATE ... WHERE id IN (SELECT ... LIMIT ?)` (`store/jobs.rs:96-104`); `BACKOFF_SECS = [30,120,600]` → `status='dead'` retaining error (`jobs.rs:17,160`); `reap_expired_claims`/`touch_claim` (D-11); test `crash_mid_job_resumes_without_duplication` (task-abort == kill -9) |
| SC4 | Workers in bounded pool sized to configured LLM capacity | ✓ VERIFIED | `spawn_workers` spawns exactly `max_concurrency` loops (`queue/mod.rs:73`); default 1 (`config.rs:159`); test `pool_is_bounded` asserts in-flight gauge reaches and never exceeds N |

## Must-Haves (aggregated from 5 PLAN frontmatters)

| Plan | Truth | Status | Evidence |
|------|-------|--------|----------|
| 01 | Bare `[llm]` parses with mock/1/300 defaults (D-02, D-04) | ✓ | `config.rs:87-90,159-163`; unit test asserts `max_concurrency==1`, `timeout_secs==300` (`config.rs:294-295`) |
| 01 | `SAUSSUNET_LLM__API_KEY` env wins over file at resolve() (D-03) | ✓ | `config.rs:168` `ENV_LLM_API_KEY` + resolve override path |
| 01 | api_key renders `***` in Debug, never in Display (D-03) | ✓ | `SecretString` manual Debug `SecretString("***")` (`config.rs:67-71`); `CoreError::Llm { status, url }` only (`error.rs:39`, no `#[from] reqwest::Error`) |
| 01 | CoreError has Llm / LlmParse / Queue variants | ✓ | `error.rs:39,42,45` |
| 01 | Committed config.toml byte-identical to default_config() incl. `[llm]` | ✓ | `config.toml:17-25` `[llm]` block; unit test `assert_eq!(committed, default_config())` (`lib.rs:184`) |
| 02 | claim_jobs atomic; N concurrent claimers never get same job (D-12) | ✓ | `jobs.rs:96` transactional claim; test `claim_is_atomic_under_concurrent_workers` |
| 02 | fail_job: attempts++ → pending +30s/+2min/+10min; 4th → dead with error (D-09/D-10) | ✓ | `jobs.rs:17` `BACKOFF_SECS=[30,120,600]`; dead path `jobs.rs:160` retains `error`; tests `retry_backoff_schedule_matches_*`, `backoff_gates_claim` |
| 02 | Future `next_attempt_at` not claimable; due → claimable | ✓ | migration `0005_jobs_next_attempt_at.sql` (`ALTER TABLE jobs ADD COLUMN next_attempt_at TEXT`); test `backoff_gates_claim` (injected clock) |
| 02 | reap_expired_claims → pending; touch_claim refreshes (D-11) | ✓ | `jobs.rs:121,192`; tests `reap_returns_expired_claims_only`, `touch_claim_extends_lease` |
| 02 | complete_job → done with finished_at | ✓ | `jobs.rs:132` |
| 03 | LlmProvider dyn-compatible; typed via free `generate_json_typed` (D-07) | ✓ | `llm/mod.rs:69-70` `#[async_trait] pub trait LlmProvider: Send + Sync`; `generate_json_typed<T: DeserializeOwned>` free fn (`mod.rs:105`) |
| 03 | Tolerant parse extracts `{...}` from noisy prose (D-05) | ✓ | `extract_json_block` (`mod.rs:122`), string-literal-aware brace scanner; llm_provider.rs tests |
| 03 | Exactly one in-job repair prompt, zero queue retries (D-06) | ✓ | `mod.rs:91-95` single repair round via `repair_prompt`; repair test asserts 2nd prompt content via `prompts` accessor |
| 03 | MockProvider scripted Ok/SlowOk/RawText/InvalidJson/Fail/Hang (D-13/D-15) | ✓ | `mock.rs:26-45` all six `Scripted` variants; `impl LlmProvider for MockProvider` (`mock.rs:128`) |
| 03 | Provider suite constructs MockProvider directly, zero config/keys (D-14) | ✓ | `tests/llm_provider.rs` (9 tests); check.sh grep guard forbids `OpenAiCompat` there |
| 04 | spawn_workers = exactly max_concurrency loops; gauge proves peak == N, never exceeds (D-04) | ✓ | `queue/mod.rs:73`; test `pool_is_bounded` (2×N jobs, SlowOk 50ms) |
| 04 | insert → claim → generate_json → done thin slice | ✓ | test `job_flows_through_worker_to_done` |
| 04 | Heartbeat refreshes claimed_at every interval; live jobs never reaped (D-11) | ✓ | select!-branch heartbeat (`queue/mod.rs` header + WorkerConfig `heartbeat_interval` 60s); test `heartbeat_keeps_live_job_unreaped` |
| 04 | Killed worker's job reaped after expiry, re-claimed, completes exactly once (SC3) | ✓ | test `crash_mid_job_resumes_without_duplication` (JoinHandle::abort == kill -9) |
| 04 | Provider failure → fail_job: retry after backoff gate, dead stays dead with error | ✓ | test `provider_failure_routes_through_backoff_then_dead` |
| 05 | OpenAiCompat POST {base_url}/chat/completions, bearer, response_format best-effort (400→retry without), non-streaming, timeout (D-01/D-05/D-08) | ✓ | `openai_compat.rs`: `chat/completions`, `bearer_auth`, `response_format` toggle (`:77-85`), `"stream": false` (`:82`), client timeout |
| 05 | CoreError::Llm Display = status+url only; key in no error/log/Debug (D-03) | ✓ | `error.rs:39`; redacted Debug + 2 secret-leak unit tests in openai_compat.rs |
| 05 | provider_from_config selects single active provider (D-02) | ✓ | `llm/mod.rs:41-64` mock / openai-compatible / error arms |
| 05 | Bare run / init-vault boots daemon: config → boot_open → provider → spawn_workers → ctrl_c → abort (D-04, SC1, SC4) | ✓ | `main.rs:129-147` (boot_open → provider_from_config → spawn_workers → ctrl_c → abort); SUMMARY records daemon smoke (queue ready line, SIGINT → exit 0) |
| 05 | #[ignore]-gated live-endpoint test, never run by CI (D-01/D-14) | ⚠️ UNCERTAIN | Test exists and is correctly gated (`llm_live.rs:27`; default run = 1 ignored; `-- --ignored` without key fails honestly) — but the *actual validation against a real endpoint* has never been executed (no credentials). Needs human run. |
| 05 | check.sh proves CI zero-real-LLM (D-14) | ✓ | `scripts/check.sh:14` grep guard + `:20` `env -u SAUSSUNET_LLM__API_KEY cargo test` |

**Artifacts:** all 10 plan-listed artifacts exist, are substantive (no stubs), and are wired (`pub mod llm; pub mod queue;` in lib.rs; main.rs consumes provider factory + spawn_workers). **Key links:** all 6 verified (claim ORDER BY pattern, `impl LlmProvider for MockProvider`/`for OpenAiCompat`, `Arc<dyn LlmProvider>` injection, `provider_from_config` in main.rs).

## Requirements Coverage

| Req | Description | Status | Proven by |
|-----|-------------|--------|-----------|
| LLMP-01 | Abstract LlmProvider, any OpenAI-compatible endpoint | ✓ SATISFIED (live run pending) | Plans 01/03/05: config, trait, OpenAiCompat, factory |
| LLMP-02 | Bounded pool sized to LLM capacity; deterministic mock testability | ✓ SATISFIED | Plans 03/04: MockProvider, pool_is_bounded |
| SRCC-03 | Durable SQLite queue: atomic claim, retry states, claimed-at expiry | ✓ SATISFIED | Plan 02/04: claim/backoff/reap + crash-resume test |

REQUIREMENTS.md traceability maps all three IDs to Phase 2 (table rows still say "Pending" — ROADMAP/STATE updates are orchestrator-owned, not modified here).

## Decision Coverage (D-01..D-14)

14/14 honored. D-01 (cloud-first live validation) is implemented as the env-gated test but its *execution* is the pending human item; all others are directly evidenced in code above (D-02 factory, D-03 SecretString/Llm variant, D-04 default 1 + bounded pool, D-05 extract + best-effort response_format, D-06 single repair, D-07 generate_json_typed, D-08 `"stream": false`, D-09 [30,120,600], D-10 status='dead' + error retained, D-11 60s heartbeat / 5min expiry, D-12 transactional claim + concurrency test, D-13 Scripted enum, D-14 check.sh guards).

## Behavioral Verification

| Check | Result |
|-------|--------|
| `cargo test -p saussunet-core` | **121 passed, 0 failed, 1 ignored** (matches expected exactly) |
| Test counts per requirement file | queue_store 7, llm_provider 9, queue 6, llm_live 1 (ignored) — match SUMMARYs |
| Disabled-test scan | Only `#[ignore]` is `llm_live.rs` — intentional per D-14, not a deception; honest-skip path documented |
| Anti-patterns (TBD/FIXME/XXX/placeholder/TODO/HACK) | None in any phase-modified file |

## Human Verification

1. **Live endpoint validation (D-01, SC1 live half)** — run once with real credentials, cloud first then Ollama (from 02-05-SUMMARY.md):
   ```bash
   SAUSSUNET_LLM__BASE_URL=https://api.openai.com/v1 SAUSSUNET_LLM__MODEL=<model> SAUSSUNET_LLM__API_KEY=*** \
     cargo test -p saussunet-core --test llm_live -- --ignored
   # then, unchanged except BASE_URL/MODEL (Ollama accepts any non-empty key):
   SAUSSUNET_LLM__BASE_URL=http://localhost:11434/v1 SAUSSUNET_LLM__MODEL=<model> SAUSSUNET_LLM__API_KEY=*** \
     cargo test -p saussunet-core --test llm_live -- --ignored
   ```
   Expected: test passes (`ok == true`). Why not programmatic: no LLM credentials exist in this environment; fabricating them is forbidden.

## Gaps

None. The single UNCERTAIN truth is the known pending item above, recorded as human_verification per instruction — it does not block the phase.

---
_Verified against actual codebase by GSD verifier, 2026-10-26. Source unmodified._
