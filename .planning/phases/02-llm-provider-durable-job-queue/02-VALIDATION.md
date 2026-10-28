---
phase: 02
slug: llm-provider-durable-job-queue
status: complete
nyquist_compliant: true
wave_0_complete: true
created: 2026-10-28
---

# Phase 2 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.
> Reconstructed retroactively from artifacts (State B): 5 PLANs (each with 2 `<verify>` blocks), 5 SUMMARYs, 127-test green suite at audit time.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | cargo test (Rust, std #[test] + tokio::test) |
| **Config file** | Cargo.toml (workspace) — no extra test config |
| **Quick run command** | `cargo test -p saussunet-core --lib 2>&1 \| tail -5` |
| **Full suite command** | `cargo test -p saussunet-core 2>&1 \| tail -5` (or `bash scripts/check.sh` for fmt+clippy+guards+release) |
| **Estimated runtime** | quick ~5s · full ~30s · check.sh ~2min |

---

## Sampling Rate

- **After every task commit:** Run `cargo test -p saussunet-core --lib`
- **After every plan wave:** Run `cargo test -p saussunet-core`
- **Before `/gsd:verify-work`:** Full suite green + `bash scripts/check.sh` → `check: OK`
- **Max feedback latency:** 30 seconds

---

## Per-Task Verification Map

| Task ID | Plan | Wave | Requirement | Threat Ref | Secure Behavior | Test Type | Automated Command | File Exists | Status |
|---------|------|------|-------------|------------|-----------------|-----------|-------------------|-------------|--------|
| 02-01-T1 | 01 | 1 | LLMP-01 | T-01, T-02 | api_key redacted in Debug; config outside vault | unit | `cargo test -p saussunet-core --lib config:: 2>&1 \| tail -5` | ✅ | ✅ green |
| 02-01-T2 | 01 | 1 | LLMP-01 | T-01 | env override, SecretString, numeric validation (WR-02) | unit | `cargo test -p saussunet-core --lib config::tests 2>&1 \| tail -5` | ✅ | ✅ green |
| 02-02-T1 | 02 | 1 | SRCC-03 | T-03 | all SQL bind-param only; claim fencing token | unit/integration | `cargo test -p saussunet-core --test queue_store 2>&1 \| tail -5` | ✅ | ✅ green |
| 02-02-T2 | 02 | 1 | SRCC-03 | T-03 | atomic claim, D-09 backoff ladder, reap, stale-claimant rejection | integration | `cargo test -p saussunet-core --test queue_store 2>&1 \| tail -5` | ✅ | ✅ green |
| 02-03-T1 | 03 | 2 | LLMP-01, LLMP-02 | T-04 | prompt = message content only; bounded repair echo | unit | `cargo test -p saussunet-core --lib llm:: 2>&1 \| tail -5` | ✅ | ✅ green |
| 02-03-T2 | 03 | 2 | LLMP-01, LLMP-02 | T-04 | scripted mock: extraction, repair, Fail/Hang/SlowOk, in-flight gauge | integration | `cargo test -p saussunet-core --test llm_provider 2>&1 \| tail -5` | ✅ | ✅ green |
| 02-04-T1 | 04 | 3 | LLMP-02, SRCC-03 | T-05, T-06 | pool spawns exactly N tasks; heartbeat in-task | unit | `cargo test -p saussunet-core --lib queue:: 2>&1 \| tail -5` | ✅ | ✅ green |
| 02-04-T2 | 04 | 3 | LLMP-02, SRCC-03 | T-05, T-06 | E2E crash-resume, no duplication, backoff→dead | integration | `cargo test -p saussunet-core --test queue 2>&1 \| tail -5` | ✅ | ✅ green |
| 02-05-T1 | 05 | 4 | LLMP-01, LLMP-02 | T-01, T-07 | URL from base_url only; key never in errors/Debug | unit | `cargo test -p saussunet-core --lib llm::openai_compat 2>&1 \| tail -5` | ✅ | ✅ green |
| 02-05-T2 | 05 | 4 | LLMP-01, LLMP-02 | T-08 | CI zero-real-LLM guard; daemon wiring smoke; mock daemon no-drain (WR-03) | integration + CI guard | `bash scripts/check.sh 2>&1 \| tail -5` | ✅ | ✅ green |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

Requirement coverage: **LLMP-01** (02-01/03/05 tasks), **LLMP-02** (02-03/04/05), **SRCC-03** (02-02/04) — every requirement has green automated verification. Suite at audit time: 127 passed / 0 failed / 1 ignored.

---

## Wave 0 Requirements

Existing infrastructure covers all phase requirements (cargo test + scripts/check.sh guards). No Wave 0 needed.

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| Live end-to-end against a real OpenAI-compatible endpoint (D-01) | LLMP-01 | D-14 design: CI must structurally never call a real LLM; live test is `#[ignore]`d and reads credentials from env only inside the test body | `SAUSSUNET_LLM__BASE_URL=... SAUSSUNET_LLM__MODEL=... SAUSSUNET_LLM__API_KEY=*** cargo test -p saussunet-core --test llm_live -- --ignored` — **PASSED 2026-10-26** against local Halogen endpoint (see 02-HUMAN-UAT.md) |

---

## Validation Sign-Off

- [x] All tasks have `<automated>` verify (2 per plan, all executed during execution)
- [x] Sampling continuity: no 3 consecutive tasks without automated verify
- [x] Wave 0 covers all MISSING references (none missing)
- [x] No watch-mode flags
- [x] Feedback latency < 30s
- [x] `nyquist_compliant: true` set in frontmatter

**Approval:** approved 2026-10-28

---

## Validation Audit 2026-10-28
| Metric | Count |
|--------|-------|
| Gaps found | 0 |
| Resolved | 0 |
| Escalated | 1 (live endpoint — by design, manual-only, already passed via HUMAN-UAT) |
