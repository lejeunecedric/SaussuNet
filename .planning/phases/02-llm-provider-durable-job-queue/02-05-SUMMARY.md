---
phase: 02-llm-provider-durable-job-queue
plan: 05
subsystem: llm-provider, daemon
tags: [rust, reqwest, openai-compatible, bearer-auth, provider-factory, tokio-signal, daemon, env-gated-test, ci-guard]

requires:
  - phase: 02-llm-provider-durable-job-queue (plan 01)
    provides: "[llm] config fields on RuntimeConfig, SecretString, CoreError::Llm { status, url }, reqwest 0.12 rustls dep"
  - phase: 02-llm-provider-durable-job-queue (plan 03)
    provides: "LlmProvider trait (dyn-compatible, D-05/D-06 default contract), MockProvider for factory mock arm"
  - phase: 02-llm-provider-durable-job-queue (plan 04)
    provides: "spawn_workers + WorkerConfig consumed verbatim by the daemon wiring"
provides:
  - "OpenAiCompat: reqwest impl of LlmProvider over POST {base_url}/chat/completions (bearer, response_format best-effort with 400-retry-without, non-streaming, client timeout)"
  - "provider_from_config(rt) — single active provider selection (D-02): mock | openai-compatible | error"
  - "Daemon mode: bare run / init-vault boots store+vault → provider → bounded worker pool → blocks until ctrl_c → aborts handles"
  - "tests/llm_live.rs — #[ignore]-gated real-endpoint validation (D-01), env-only credentials"
  - "scripts/check.sh zero-real-LLM proof: grep guard + env -u SAUSSUNET_LLM__API_KEY cargo test (D-14)"
affects: [phase-3-extraction, deployment, provider-factory, ci]

tech-stack:
  added: []
  patterns:
    - "Secret-safe HTTP impl: api_key revealed only into bearer_auth at send time; non-2xx errors carry status+url with NO response-body interpolation (bodies can echo request headers)"
    - "Best-effort capability flag: request response_format, on 400 retry the identical request once without it (local llama.cpp/Ollama compat)"
    - "Env-gated live test: #[ignore] + env reads inside the test body only; CI structurally cannot reach a real endpoint"

key-files:
  created:
    - crates/saussunet-core/src/llm/openai_compat.rs
    - crates/saussunet-core/tests/llm_live.rs
  modified:
    - crates/saussunet-core/src/llm/mod.rs
    - crates/saussunet/src/main.rs
    - scripts/check.sh

key-decisions:
  - "OpenAiCompat::new returns Result — reqwest ClientBuilder::build() can fail (TLS backend init); mapped to CoreError::Config, plan left the return type open"
  - "Transport errors and malformed success bodies surface as CoreError::Llm { status: 0, url: \"<url>: <message>\" } — the variant (D-03 minimalism) has no message field, so the notice rides in url (same precedent as 02-03 mock script-exhaustion); reqwest Error Display never contains the header map"
  - "check.sh guard uses the if-form (mirrors the existing axum guard) instead of the plan's `&& {...} || true` sketch — passes when grep finds nothing, fails loudly otherwise"
  - "Daemon prints 'queue ready: provider={name} workers={n}' after 'vault ready' and 'shutdown complete' after ctrl_c; bare run and init-vault share the boot path (SC1 unchanged)"

requirements-completed: [LLMP-01, LLMP-02]

duration: 5min
completed: 2026-10-26
---

# Phase 2 Plan 05: Real Provider + Daemon Wiring Summary

**`OpenAiCompat` speaks POST {base_url}/chat/completions with bearer auth, best-effort `response_format` (400 → one retry without it) and a client-enforced timeout — errors carry status+url only, never the key — `provider_from_config` selects the single active provider, and the binary is now a daemon: boot_open → provider → bounded worker pool → ctrl_c → abort; live-endpoint test exists env-gated but was NOT executed (no credentials in this environment).**

## Performance

- **Duration:** 5 min
- **Started:** 2026-10-26T08:26:22Z
- **Completed:** 2026-10-26T08:29:06Z (SUMMARY commit shortly after)
- **Tasks:** 2
- **Files modified:** 5 (2 created, 3 modified)

## Accomplishments
- `OpenAiCompat` (openai_compat.rs): `POST {base_url}/chat/completions`, `authorization: Bearer <key>` built at send time only, body `{"model", "messages":[{role:user}], "stream": false, "response_format":{"type":"json_object"}}` (D-08); on HTTP 400 the identical request is retried once WITHOUT `response_format` (D-05 best-effort for llama.cpp/Ollama); non-2xx → `CoreError::Llm { status, url }` with no body echo (D-03); reqwest transport/timeout errors → `Llm { status: 0, url: "<url>: <transport msg>" }`; base_url trailing slash trimmed at construction so untrusted payload content can never influence the URL (SSRF bound). Overrides only `complete` — the whole D-05 tolerant-parse + D-06 repair contract is inherited from the trait default.
- D-03 secrecy proven: manual Debug prints `api_key: "***"`; unit tests assert a fake key literal (`test-pass-9f3a`) and "Bearer" are absent from `CoreError::Llm` Display/Debug and from the provider's Debug. `grep -n api_key openai_compat.rs` shows the key only in the private field, the redacted Debug, the constructor reveal, and `bearer_auth`.
- `provider_from_config(&RuntimeConfig)` in llm/mod.rs (D-02): "mock" → empty-script MockProvider (plumbing-only daemon mode), "openai-compatible" → OpenAiCompat with required api_key (missing → `CoreError::Config("llm api_key required: set [llm].api_key or SAUSSUNET_LLM__API_KEY")`), unknown → `CoreError::Config`.
- Daemon wiring in main.rs bare-run/init-vault boot path: `boot_open` → `Arc::new(store)` → `provider_from_config` → `WorkerConfig { max_concurrency: rt.llm_max_concurrency, ..Default::default() }` (D-04 bound from config) → `spawn_workers` → prints `queue ready: provider=mock workers=1` → `tokio::signal::ctrl_c()` → abort handles → `shutdown complete`. Other subcommands (init/version/write-page/conflicts/reindex) unchanged.
- `tests/llm_live.rs`: single `#[ignore = "requires SAUSSUNET_LLM__API_KEY + SAUSSUNET_LLM__BASE_URL + SAUSSUNET_LLM__MODEL"]` tokio test constructing OpenAiCompat from env vars only and asserting `generate_json("Return exactly this JSON: {\"ok\": true}", &json!({}))` yields `ok == true` — the D-01 cloud-first validation path, identical for Ollama.
- CI zero-real-LLM proof (D-14) in scripts/check.sh: grep guard forbids `OpenAiCompat` in tests/llm_provider.rs and tests/queue.rs, and the test step runs as `env -u SAUSSUNET_LLM__API_KEY cargo test`.

## Task Commits

Each task was committed atomically:

1. **Task 1: OpenAiCompat HTTP impl + provider factory** — `5f19dfc` (feat)
2. **Task 2: Daemon wiring + env-gated live test + CI zero-key proof** — `2edc3f8` (feat)

**Plan metadata:** this SUMMARY commit (docs)

## Files Created/Modified
- `crates/saussunet-core/src/llm/openai_compat.rs` — `OpenAiCompat` reqwest impl of `LlmProvider` + redacted Debug + 2 secret-leak unit tests
- `crates/saussunet-core/src/llm/mod.rs` — `pub mod openai_compat` + re-export + `provider_from_config` factory (D-02)
- `crates/saussunet/src/main.rs` — bare-run/init-vault boot path is now the daemon (boot_open + provider + spawn_workers + ctrl_c shutdown)
- `crates/saussunet-core/tests/llm_live.rs` — env-gated `#[ignore]`d live-endpoint validation (D-01)
- `scripts/check.sh` — zero-real-LLM grep guard + `env -u SAUSSUNET_LLM__API_KEY cargo test`

## Decisions Made
- `OpenAiCompat::new(...) -> Result<Self>`: reqwest client build failure mapped to `CoreError::Config` (plan left the return type open).
- Error notice for transport failures and malformed success bodies rides in the `url` field of `CoreError::Llm { status: 0, .. }` — the D-03-minimal variant has no message field; same precedent as 02-03's mock script-exhaustion.
- check.sh guard written in if-form (consistent with the existing axum guard) rather than the plan's `&& { } || true` sketch; same semantics.
- Live test uses a 120 s client timeout (generous for cold local models); production default stays `timeout_secs` (300) from config.

## Deviations from Plan

None - plan executed exactly as written. (The three decisions above fill details the plan left open; no acceptance criterion was altered.)

## Issues Encountered
- No LLM credentials exist in this environment (`env | grep -i SAUSSUNET` → nothing set), so the live-endpoint validation (D-01, SC1 live half) could NOT be executed here. The test is committed and correctly gated: default `cargo test` reports `0 passed; 1 ignored`; `-- --ignored` without a key fails with the explicit "skipped: SAUSSUNET_LLM__API_KEY not set" panic — never a fake pass.

## Verification Evidence
- `cargo test -p saussunet-core`: **121 passed, 0 failed, 1 ignored** (was 119 at plan start: +2 openai_compat secret-leak unit tests) — run under `env -u SAUSSUNET_LLM__API_KEY` inside check.sh
- `cargo test -p saussunet-core --test llm_live`: **0 passed; 0 failed; 1 ignored** — CI executes zero live tests
- `cargo test -p saussunet-core --test llm_live -- --ignored` (no key): fails with "skipped: SAUSSUNET_LLM__API_KEY not set" — honest skip path, no fabricated credential
- Daemon smoke: `./target/debug/saussunet` printed `vault ready: .../data/vaults/1` + `queue ready: provider=mock workers=1`, stayed alive, and on SIGINT printed `shutdown complete` with exit 0
- `bash scripts/check.sh`: **check: OK** (fmt, clippy -D warnings all targets, zero-real-LLM guard, tests with key env unset, axum-absence guard, release build)
- Acceptance greps: openai_compat.rs contains `chat/completions` / `"response_format"` / `"stream": false` / `bearer_auth` / `CoreError::Llm { status` with no body interpolation; llm/mod.rs has `pub fn provider_from_config` matching `"mock"` and `"openai-compatible"`; main.rs boot path has `boot_open` + `Arc::new(store)` + `provider_from_config` + `spawn_workers` + `tokio::signal::ctrl_c`; check.sh has the guard and `env -u SAUSSUNET_LLM__API_KEY cargo test`; `grep -n api_key openai_compat.rs` → key only in field/redacted-Debug/reveal/bearer_auth

## User Setup Required
**SC1 live validation is pending a user run** (D-01 order: cloud first, then Ollama — same code path):
```bash
SAUSSUNET_LLM__BASE_URL=https://api.openai.com/v1 SAUSSUNET_LLM__MODEL=<model> SAUSSUNET_LLM__API_KEY=*** \
  cargo test -p saussunet-core --test llm_live -- --ignored
# then, unchanged except BASE_URL/MODEL (Ollama accepts any non-empty key):
SAUSSUNET_LLM__BASE_URL=http://localhost:11434/v1 SAUSSUNET_LLM__MODEL=<model> SAUSSUNET_LLM__API_KEY=*** \
  cargo test -p saussunet-core --test llm_live -- --ignored
```

## Next Phase Readiness
- Phase 3 (extraction): job kinds/payloads plug into the existing worker prompt; `provider_from_config` + `spawn_workers` are the production seams — Phase 3 only defines payload schemas, no plumbing changes.
- Phase 5 UI can surface queue state via `list_jobs(status="dead")` etc.; the daemon already owns the pool lifecycle.
- Remaining Phase 2 checklist item for the orchestrator/verify step: the manual `-- --ignored` live run above (needs a real key; CI must never run it).

## Self-Check: PASSED

---
*Phase: 02-llm-provider-durable-job-queue*
*Completed: 2026-10-26*
