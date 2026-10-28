---
phase: 02
slug: llm-provider-durable-job-queue
status: complete
threats_open: 0
asvs_level: 1
created: 2026-10-28
---

# Phase 2 — Security

> Per-phase security contract: threat register, accepted risks, and audit trail.
> Register authored at plan time (all 5 PLAN files carried `<threat_model>` blocks, OWASP-ASVS-L1, block=high).

---

## Trust Boundaries

| Boundary | Description | Data Crossing |
|----------|-------------|---------------|
| operator config → daemon | `config.toml` (gitignored runtime file) supplies `[llm]` provider, base_url, api_key | api_key (secret), base_url (operator-trusted endpoint) |
| untrusted source content → LLM prompt | Phase 3 will build prompts from watched source content; Phase 2 defines the contract: payload enters only the user-message body | untrusted text → model context |
| daemon → external LLM endpoint | reqwest HTTPS call, bearer auth | prompt content, api_key header |
| CI ↔ repo | `check.sh` must never call a real LLM nor leak secrets | test fixtures only |

---

## Threat Register

| Threat ID | Category | Component | Disposition | Mitigation | Status |
|-----------|----------|-----------|-------------|------------|--------|
| T-01 | Information disclosure (D-03) | config.rs / error.rs / openai_compat.rs | mitigate | `SecretString` redacts Debug; `CoreError::Llm` carries only {status,url}; non-2xx path drops response body (bodies can echo request headers); `OpenAiCompat` manual Debug prints `api_key: "***"`. Tests: `debug_output_never_leaks_api_key`, `llm_error_display_and_debug_never_leak_api_key`, `provider_debug_redacts_api_key` | closed |
| T-02 | Data exposure (carry-over T-01-01) | config.toml placement | mitigate | Vault half: `assert_outside_vault` on every boot path (main.rs:127/176/211/244). Gitignore half: **initially broken** (config.toml was tracked while claimed gitignored — auditor ESCALATE). Fixed in `26f6413`: runtime `config.toml` gitignored + untracked, secret-free template ships as tracked `config.example.toml`, byte-identity test retargeted (`committed_example_config_matches_default_config`) | closed |
| T-03 | Injection (SQL) | store/jobs.rs (+migrations 0005/0006) | mitigate | All SQL static string literals with `?` binds only (insert/list/claim/touch/complete/fail/reap); no `format!`-built SQL; migrations are static ALTER TABLE | closed |
| T-04 | Injection (prompt) | llm/mod.rs, queue/mod.rs, openai_compat.rs | mitigate | Payload enters only `{"role":"user","content":...}` — no system role; repair echo bounded `REPAIR_ECHO_MAX_CHARS=4000`; output only serde_json-parsed into typed T; no Command/spawn/URL-fetch in llm/ or queue/ | closed |
| T-05 | Denial of service (D-04) | queue/mod.rs, store/jobs.rs, openai_compat.rs, config.rs | mitigate | `spawn_workers` spawns exactly max_concurrency tasks; `claim_jobs LIMIT ?`; reap bounded by WHERE window; reqwest client timeout enforced; WR-02 rejects `max_concurrency=0` / `timeout_secs=0` at resolve. Tests: `pool_is_bounded`, `zero_max_concurrency_is_rejected`, `zero_timeout_secs_is_rejected`, `hang_delays_then_fails` | closed |
| T-06 | Tampering / DoS (stuck worker) | queue/mod.rs heartbeat + reap | mitigate | Heartbeat as in-task `tokio::select!` branch (abort == heartbeat stops); reap-first each loop; expiry default 300s. Tests: `heartbeat_keeps_live_job_unreaped`, `crash_mid_job_resumes_without_duplication`, `touch_claim_extends_lease`, `concurrent_workers_no_duplication` | closed |
| T-07 | Spoofing (SSRF) | openai_compat.rs | mitigate | URL built solely from operator `base_url` at construction (`{base_url}/chat/completions`); prompt/payload can never influence the URL; trailing-slash trim test | closed |
| T-08 | Information disclosure (CI, D-14) | tests/llm_live.rs, scripts/check.sh | mitigate | Live test `#[ignore]`d, env read only inside test body; check.sh runs `env -u SAUSSUNET_LLM__API_KEY cargo test` and greps mock-only tests for forbidden tokens with hard fail | closed |
| T-02b | Data integrity (02-02 register + WR-01) | store/jobs.rs claim fencing | mitigate | Migration 0006 `claim_token` (`lower(hex(randomblob(16)))` per claim); complete/fail/touch fenced `AND status='claimed' AND claim_token=?`; dead-letter retains error + payload_json; reap touches only claimed rows. Test: `stale_claimant_cannot_clobber_live_claim` | closed |

*Status: open · closed*
*Disposition: mitigate (implementation required) · accept (documented risk) · transfer (third-party)*

---

## Accepted Risks Log

| Risk ID | Threat Ref | Rationale | Accepted By | Date |
|---------|------------|-----------|-------------|------|

*Accepted risks do not resurface in future audit runs.*

---

## Audit Trail

## Security Audit 2026-10-28
| Metric | Count |
|--------|-------|
| Threats found | 9 (register groups; 14 plan-time entries) |
| Closed | 9 |
| Open | 0 |

- Verify-only audit against plan-time threat models (register_authored_at_plan_time: true), ASVS L1, block_on=high.
- 8/9 closed on first pass with file:line + test evidence (auditor ran the cited tests).
- T-02 escalated: mitigation claim "config.toml is gitignored" contradicted by implementation (file tracked in `2272ddc`/`883962f`/`bd9f7cc`; repo is PUBLIC). User chose fix over accept.
- T-02 remediated in `fix(02): T-02 untrack runtime config.toml...` (`26f6413`): config.toml gitignored + untracked, config.example.toml template tracked, test retargeted. Full gate re-run: `check: OK` (127 tests green).
- Prior code review (02-REVIEW.md) had repeated the false "gitignored-template" claim — lesson: verify git-tracking state of secret-bearing config paths, don't trust comments.
