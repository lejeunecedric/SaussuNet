---
phase: 03
slug: extraction-pipeline
status: verified
nyquist_compliant: true
wave_0_complete: true
created: 2026-10-30
---

# Phase 3 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.
> Reconstructed post-execution (State B) from 8 PLAN/SUMMARY artifacts; all commands
> re-run green on 2026-10-30 at HEAD `56d8c92` (214 tests workspace-wide, 0 failed).

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | cargo test (Rust, per-crate integration tests + `#[cfg(test)]` unit tests) |
| **Config file** | none — `Cargo.toml` workspace + `scripts/check.sh` gate |
| **Quick run command** | `cargo test -p saussunet-core --test phase3_acceptance 2>&1 \| tail -5` |
| **Full suite command** | `bash scripts/check.sh` (fmt, clippy -D warnings, full test suite, zero-real-LLM guard, axum-absence guard, PIPE-06 write-funnel guard, release build) |
| **Estimated runtime** | quick ~5 s · full gate ~120–180 s (cold cargo longer) |

---

## Sampling Rate

- **After every task commit:** `cargo test -p saussunet-core --test <targeted-suite>` (suite per task below)
- **After every plan wave:** `cargo test --workspace` (aggregate via `grep '^test result' | awk '{p+=$4;f+=$6} END{print p,f}'`)
- **Before `/gsd-verify-work`:** `bash scripts/check.sh` must be green
- **Max feedback latency:** ~180 s (full gate); targeted suites < 10 s

---

## Per-Task Verification Map

| Task ID | Plan | Wave | Requirement | Threat Ref | Secure Behavior | Test Type | Automated Command | File Exists | Status |
|---------|------|------|-------------|------------|-----------------|-----------|-------------------|-------------|--------|
| 03-01-01 | 01 | 1 | PIPE-01 | T-03-01 | SSRF guard rejects private/loopback/metadata URLs pre-request; userinfo stripped | unit | `cargo test -p saussunet-core --lib pipeline::fetch` (7 tests) | ✅ | ✅ green |
| 03-01-02 | 01 | 1 | PIPE-01 | T-03-05 | raw/ written only via `write_raw` → `atomic::write_atomic`; unknown job kinds dead-letter | integration | `cargo test -p saussunet-core --test pipeline_url` | ✅ | ✅ green |
| 03-02-01 | 02 | 2 | PIPE-01 | T-03-06 | ingest route validates URL (400, no job row); 64 KiB body cap | integration | `cargo test -p saussunet --test http_ingest` | ✅ | ✅ green |
| 03-02-02 | 02 | 2 | PIPE-01 | T-03-03 | logs carry method/path/status/job-id only; base_path normalized | integration | `cargo test -p saussunet --test http_ingest` + `cargo test -p saussunet-core --lib config` | ✅ | ✅ green |
| 03-03-01 | 03 | 2 | PIPE-03, PIPE-04, PIPE-05 | T-03-04 | prompts wrap source as UNTRUSTED DATA; typed schemas bound output | unit | `cargo test -p saussunet-core --lib pipeline` (schemas/prompts) | ✅ | ✅ green |
| 03-03-02 | 03 | 2 | PIPE-01, PIPE-03, PIPE-04, PIPE-05 | T-03-05 | two-pass extraction: threshold gate, dedup, provenance, contested — pages only via funnel | integration | `cargo test -p saussunet-core --test pipeline_extract` | ✅ | ✅ green |
| 03-04-01 | 04 | 3 | PIPE-02 | T-03-03 | drift log entry carries URL + old/new sha256 only | integration | `cargo test -p saussunet-core --test pipeline_drift` | ✅ | ✅ green |
| 03-04-02 | 04 | 3 | PIPE-02 | — | unchanged source = zero-LLM no-op (`provider.call_count() == 0`); sha stored post-extraction | integration | `cargo test -p saussunet-core --test pipeline_drift` | ✅ | ✅ green |
| 03-05-01 | 05 | 4 | PIPE-01 | T-03-07 | pdftotext fixed flags no shell; missing binary fails LOUD with install hint | integration | `cargo test -p saussunet-core --test pipeline_pdf` | ✅ | ✅ green |
| 03-05-02 | 05 | 4 | PIPE-01 | T-03-02 | upload guards: 413 oversized, 400 magic/content-type, traversal filename sanitized | integration | `cargo test -p saussunet --test http_upload` | ✅ | ✅ green |
| 03-06-01 | 06 | 3 | PIPE-06 | T-03-05 | validator rejects off-whitelist links, missing provenance, wiki-as-source | unit | `cargo test -p saussunet-core --test validator` | ✅ | ✅ green |
| 03-06-02 | 06 | 3 | PIPE-05, PIPE-06 | T-03-04 | poisoned LLM output never reaches disk; rejections recorded as `validation_rejected` proposals | integration | `cargo test -p saussunet-core --test pipeline_extract` | ✅ | ✅ green |
| 03-07-01 | 07 | 4 | PIPE-04 | — | normalize_name punctuation/plural folding; alias dedup case-insensitive | unit | `cargo test -p saussunet-core --lib entities` | ✅ | ✅ green |
| 03-07-02 | 07 | 4 | PIPE-03, PIPE-04 | — | threshold matrix + dedup corpus e2e; passing mentions never create pages | integration | `cargo test -p saussunet-core --test pipeline_thresholds --test pipeline_dedup` | ✅ | ✅ green |
| 03-08-01 | 08 | 5 | PIPE-01..06 | all | phase acceptance sc1–sc5 via real queue path (per-test isolated DBs) | e2e | `cargo test -p saussunet-core --test phase3_acceptance` (5 tests) | ✅ | ✅ green |
| 03-08-02 | 08 | 5 | PIPE-01..06 | T-03-01/03/04/05 | runtime SSRF matrix, prompt-injection containment, no-secrets-in-logs; permanent CI write-funnel guard (RED-proofed) | e2e + CI | `cargo test -p saussunet-core --test security_hygiene` + `bash scripts/check.sh` | ✅ | ✅ green |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

**Requirement coverage summary (re-run 2026-10-30):** PIPE-01 → pipeline_url, pipeline_pdf, http_ingest, http_upload · PIPE-02 → pipeline_drift, phase3_acceptance::sc2 · PIPE-03 → pipeline_thresholds, pipeline_extract, sc3 · PIPE-04 → pipeline_dedup, pipeline_extract, sc3 · PIPE-05 → pipeline_extract, sc4 · PIPE-06 → validator, security_hygiene, sc5 + permanent `check.sh` grep guard. Phase-3 suites: 46 passed / 0 failed; HTTP suites: 11 passed / 0 failed; sc1–sc5: 5/5.

---

## Sampling Rate Notes

- Known pre-existing flake (pre-dates phase, reproduced on earlier commits): `pipeline_url::unknown_kind_dead_letters_*` and `queue::provider_failure_routes_through_backoff_then_dead` intermittently hit SQLITE_BUSY — re-run on "database is locked"; new phase-3 suites use per-test isolated DBs and did not reproduce. Follow-up candidate: `BEGIN IMMEDIATE` in store claim/fail paths.

---

## Wave 0 Requirements

*None — existing infrastructure (cargo test + `scripts/check.sh` guards) covered all phase requirements; no test-framework install was needed.*

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| Real-LLM extraction quality | PIPE-03/05 (soft quality) | All CI tests use MockProvider by design (zero-real-LLM guard); output quality is model-dependent | Point config at Halogen (`http://127.0.0.1:8731/v1`) or another endpoint, ingest a real URL, inspect generated pages |
| `poppler-utils` present on deploy host | PIPE-01 (PDF leg) | Host-environment precondition, not code behavior | `command -v pdftotext` on the target host; absence fails jobs LOUD with install hint (tested) |
| Generated page renders cleanly in Obsidian | PIPE-05 | Visual/frontmatter rendering | Open a generated entity page in Obsidian; check frontmatter, wikilinks, `^[raw/...]` markers |

---

## Validation Sign-Off

- [x] All tasks have `<automated>` verify or Wave 0 dependencies
- [x] Sampling continuity: no 3 consecutive tasks without automated verify
- [x] Wave 0 covers all MISSING references (none missing)
- [x] No watch-mode flags
- [x] Feedback latency < 180s
- [x] `nyquist_compliant: true` set in frontmatter

**Approval:** approved 2026-10-30 (retroactive audit — all 16 task rows green against HEAD `56d8c92`)
