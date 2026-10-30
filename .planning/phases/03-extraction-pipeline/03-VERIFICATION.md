---
phase: 3
name: extraction-pipeline
status: passed
score: "5/5 success criteria verified with automated evidence; 6/6 requirements traced"
date: 2026-10-30
verified_by: gsd-phase-verifier
plans_verified: [03-01, 03-02, 03-03, 03-04, 03-05, 03-06, 03-07, 03-08]
---

# Phase 3 Verification: Extraction Pipeline

**Goal under test:** "Submitting a web URL or PDF produces entity and concept pages in the vault — threshold-gated, deduplicated, provenance-carrying, and validated before they touch disk."

**Verification method:** targeted `cargo test` runs on the phase acceptance + per-plan suites, `scripts/check.sh`, grep-level key-link wiring checks in `crates/saussunet-core/src/`, and spot-checks of SUMMARY claims against `git log`/`git show`. Read-only; no source modified.

## Test execution evidence (actual runs, 2026-10-30)

| Suite | Result |
|---|---|
| `phase3_acceptance` (sc1..sc5) | 5 passed, 0 failed |
| `pipeline_url`, `pipeline_extract`, `pipeline_drift`, `pipeline_pdf`, `validator`, `pipeline_thresholds`, `pipeline_dedup`, `security_hygiene` | 41 passed, 0 failed (3+4+5+7+5+2+3+12) |
| `saussunet --test http_ingest / http_upload` | 11 passed, 0 failed (6+5) |
| `bash scripts/check.sh` | `check: OK` (incl. axum-absence + write-funnel PIPE-06 guards) |

The acceptance suite runs the **real queue path** (`insert_job` → `spawn_workers(MockProvider)` → poll done), not a shortcut around the queue (`phase3_acceptance.rs:3,130-142`).

## Must-haves verification table

| # | Truth (ROADMAP SC / plan must_have) | Verified how | Evidence |
|---|---|---|---|
| SC1 | Submit URL/PDF → raw/ text with `source_url`/`ingested`/`sha256` frontmatter; entity/concept pages created/updated | `sc1_submit_url_and_pdf_produce_pages` passes; HTTP routes exist | `phase3_acceptance.rs:239`; `http_ingest.rs` (6 tests), `http_upload.rs` (5 tests) pass; `pipeline::submit_pdf`/`ingest_pdf`/`ingest_url` dispatched in `pipeline/mod.rs:114-120`; D-07 raw naming + frontmatter in `vault/raw.rs` + `pipeline_url.rs` |
| SC2 | Unchanged source = no-op (sha256 match); changed source flagged as drift | `sc2_unchanged_noop_changed_drift` + `pipeline_drift` (5 tests) pass | sha256 column via `migrations/0007_pipeline_source_sets.sql:5`; drift log.md entry emitted in `pipeline/mod.rs:315-339` (`## [date] drift \| …`, `- sha256 old -> new`, `- re-ingested …`) per D-08 |
| SC3 | Pages only above thresholds (central-to-one OR 2+ sources); passing mentions never create pages; near-dupes prevented by registry | `sc3_thresholds_and_dedup` + `pipeline_thresholds` (2) + `pipeline_dedup` (3) pass | Threshold rule `central \|\| count(entity_sources) >= min_sources` (03-03 decision, D-10/D-14/D-17); `min_sources` config default (`config.rs:104`); D-15 `normalize_name` strips punctuation + folds simple plurals (`store/entities.rs:167-169`); D-16 alias merge into `aliases_json` (`entities.rs` upsert) |
| SC4 | Pages carry sources frontmatter + `^[raw/...]` markers; contradictions flagged `contested`, never auto-resolved | `sc4_provenance_and_contested` + `pipeline_extract` (7) pass | `pages.rs:302` writes `contested: Some(true)` from pass-2 output (D-12 keeps both claims); provenance marker convention D-07/D-11 enforced by validator |
| SC5 | Every page reaching disk passed the deterministic validator; LLM never writes vault directly | `sc5_validator_gates_every_page` + `validator` (5) pass; grep proof | `vault/writer.rs:300-306` `write_generated_page` runs `validate::validate_generated_page` BEFORE `write_page`; `grep 'write_page(' src/pipeline/` shows the ONLY pipeline write is `write_generated_page` (`pages.rs:170`) — zero direct `write_page` calls; permanent CI guard `write-funnel guard (PIPE-06)` in `scripts/check.sh:28-33` (no `fs::write`/`File::create` outside `vault/atomic.rs`/`vault/raw.rs`) |
| — | Queue dispatch wired to pipeline | grep | `queue/mod.rs:161` `pipeline::handle_job(store, vault, provider, &job, &claim_token)` (D-02) |
| — | Security hygiene (03-08 must_haves) | `security_hygiene` (12 tests) pass | SSRF block-matrix asserted at runtime (`security_hygiene.rs:26-45`); `prompt_injection_contained` test (line 131) proves injection fixture sits inside the `UNTRUSTED DATA` envelope (`prompts.rs:31`) and poisoned output is validator-bounded; no `payload_json`/`api_key` logging audit (line 264-324) |

## Requirement traceability (PIPE-01..PIPE-06)

| Req | Plan frontmatter coverage | Test evidence | Status |
|---|---|---|---|
| PIPE-01 | 03-01, 03-02, 03-03, 03-05, 03-08 | sc1; pipeline_url, http_ingest, http_upload, pipeline_pdf | ✅ covered |
| PIPE-02 | 03-04, 03-08 | sc2; pipeline_drift | ✅ covered |
| PIPE-03 | 03-03, 03-07, 03-08 | sc3; pipeline_thresholds | ✅ covered |
| PIPE-04 | 03-03, 03-07, 03-08 | sc3; pipeline_dedup | ✅ covered |
| PIPE-05 | 03-03, 03-06, 03-08 | sc4; validator, pipeline_extract | ✅ covered |
| PIPE-06 | 03-06, 03-08 | sc5; validator; check.sh write-funnel guard | ✅ covered |

All six IDs appear in `.planning/REQUIREMENTS.md` (lines 19-24) and in at least one plan's `requirements:` frontmatter. No unaccounted IDs.

## SUMMARY claims vs git reality (spot-checks)

- `9364e48 test(03-08): phase3_acceptance sc1..sc5 end-to-end via real queue path` → `git show --stat`: `phase3_acceptance.rs` +615 lines — matches SUMMARY artifact claim.
- `99b96c4 test(03-08): security_hygiene runtime gates + permanent write-funnel check.sh guard` → `security_hygiene.rs` +334, `scripts/check.sh` +10 — matches.
- 03-01 SUMMARY test names (`ip_classification`, `ssrf_guard_rejects_private_targets`) exist in `pipeline/fetch.rs:241,263`.
- 32 commits reference plans 03-01..03-08; all 8 SUMMARY.md files present; HEAD `082f338 docs(phase-03): update tracking after wave 5`.

## Locked decisions D-01..D-17 (in-scope spot-checks)

D-01 (HTTP in binary only; core zero-axum — CI guard passes), D-02 (single job handler `handle_job`), D-04 (PDF copied to raw/ at submit, drift N/A for uploads — `pipeline/mod.rs` payload schema), D-06 (`CoreError::MissingBinary` LOUD hint, `error.rs:67`, `extract.rs:45`), D-07 (raw naming/frontmatter), D-08 (drift flow + log.md entry), D-11 (validator enforces markers, `validate.rs:131`), D-12 (contested kept, not resolved), D-13 (`base_path` config, `config.rs:33-34`), D-14 (`min_sources` config), D-15 (deterministic normalize: punctuation + plurals, no fuzzy), D-16 (alias merge loop), D-17 (`entity_sources` table, migration 0007) — all honored in code. D-03/D-05/D-09/D-10 exercised via passing pipeline suites (fetch retry via queue, readability+htmd, two-pass schemas).

## Human verification items (optional, non-blocking)

1. **Real LLM endpoint smoke test:** all phase tests use MockProvider by design (Phase 2 D-14 CI stance). One manual run against a configured OpenAI-compatible endpoint (Ollama/cloud) would confirm pass-1/pass-2 JSON parses against real model output.
2. **pdftotext present at deploy time:** D-06 fails LOUD but requires poppler-utils on the host; confirm on the Strix Halo box before first real PDF ingest.
3. **Obsidian eyeball:** open a generated entity page in Obsidian to confirm frontmatter/wikilinks render as expected (validator is deterministic but aesthetic check is human).

## Gaps

None blocking. One housekeeping note: `.planning/REQUIREMENTS.md` still shows PIPE-01..06 as `- [ ]` / "Pending" in the traceability table (lines 19-24, 115-120) even though the phase is complete — status flip belongs to the phase-close/tracking step, not to goal achievement.

## Verdict

**PASSED.** All 5 ROADMAP Phase 3 success criteria have concrete, executable automated evidence (sc1..sc5 green via the real queue path), all 6 PIPE requirements are traced to plans and tests, the write funnel is grep- and CI-enforced, and locked decisions D-01..D-17 are honored in code.
