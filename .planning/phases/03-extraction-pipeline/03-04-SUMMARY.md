---
phase: 03-extraction-pipeline
plan: 04
subsystem: pipeline
one_liner: "ingest_url now compares the fetched sha256 against sync_state before any write: unchanged source completes as a zero-LLM, zero-write no-op; a changed source logs a '## [date] drift |' entry with old->new digests, overwrites raw/, and re-enters the pass-2 merge path — with the digest stored only AFTER extraction so a crashed job re-ingests instead of silently no-opping."
tags: [sha256, source-drift, no-op, sync_state, log-md, mockprovider, pitfall-1, pipe-02]

requires:
  - phase: 03-extraction-pipeline/03-01
    provides: [ingest_url flow, fetch seam set_fetcher_override, write_raw, JOB_KIND_INGEST_URL]
  - phase: 03-extraction-pipeline/03-03
    provides: [sync_state.sha256 column + get/set_source_sha256, run_extraction_passes, entity_sources INSERT OR IGNORE, pipeline_extract test fixtures]
  - phase: 01-workspace-foundations-vault-writer
    provides: [vault::log::append_entry with rotation, writer::sanitize_for_nav log-injection guard]
provides:
  - "PIPE-02 no-op: sha256 match vs sync_state completes the job with zero LLM calls and zero vault writes (ROADMAP SC2)"
  - "D-08 drift branch: log.md '## [YYYY-MM-DD] drift | <title>' entry with '- sha256 <old> -> <new>' + '- re-ingested <raw path>', then normal merge-path re-extraction"
  - "crash-safe digest persistence: set_source_sha256 moved to AFTER run_extraction_passes"
  - "tests/pipeline_drift.rs: URL-keyed fetch stub + 4 end-to-end proofs (no-op / drift / no-double-count / first-seen)"
affects: [03-05, 03-06, phase-4-watcher]

tech-stack:
  added: []
  patterns:
    - "digest-compare-after-fetch, skip-the-expensive-half: fetch is cheap, LLM is expensive — the no-op gate sits between them"
    - "state-of-record written only after the expensive side effect succeeds (crash between write and extraction re-ingests, never silently no-ops)"

key-files:
  created:
    - crates/saussunet-core/tests/pipeline_drift.rs
  modified:
    - crates/saussunet-core/src/pipeline/mod.rs
    - crates/saussunet-core/src/vault/writer.rs

key-decisions:
  - "No-op return sits after fetch but BEFORE write_raw (sha256 only known post-fetch); job completes Ok(()) with zero LLM calls and zero writes"
  - "Drift entry subject reuses writer.rs sanitize_for_nav (made pub(crate)) — a crafted page title cannot forge log.md entries (T-02-05 guard reused, no duplicated helper)"
  - "set_source_sha256 moved from before run_extraction_passes to after it (plan Task 1.4): mid-extraction crash re-claim must re-ingest, not no-op"
  - "Drift log entry carries only title/URL + old/new digests + raw path — never page content (Phase 2 D-03 no-secrets-in-logs pattern)"

patterns-established:
  - "URL-keyed fetch stub (global Mutex<HashMap<url, content>>) lets one test binary swap served content between ingest runs — the fixture 03-05 drift-adjacent tests can reuse"
  - "Fresh MockProvider with EMPTY script as a zero-LLM-call proof: any call errors the job, call_count()==0 is the token-burn assertion"

requirements-completed: [PIPE-02]
requirements_completed: [PIPE-02]

duration: ~40min
completed: 2026-10-30
---

# Phase 3 Plan 04: Source drift & sha256 no-op Summary

**Re-submitting an unchanged URL now completes the job as a provable no-op — zero LLM calls, zero vault writes — while a changed sha256 appends a loud `## [date] drift |` log.md entry with old→new digests, overwrites the raw/ record, and re-enters the pass-2 merge path; the digest is persisted only after extraction succeeds so a crashed job re-ingests instead of silently no-opping (PIPE-02, ROADMAP SC2).**

## Performance

- **Duration:** ~40 min
- **Started:** 2026-10-30T12:05Z (approx)
- **Completed:** 2026-10-30T12:44Z
- **Tasks:** 2 (TDD: RED observed before GREEN)
- **Files created/modified:** 3

## What Was Built

### Task 1 — no-op + drift branch in `ingest_url` (`559de2d`)
- `pipeline/mod.rs`: after `fetcher().fetch(...)` returns `result.sha256`, `store.get_source_sha256(vault.id, &source_id)` is compared:
  - `prev == Some(same)` → `tracing::info!(source_id, "unchanged source — no-op")` and `return Ok(())` **before** `write_raw` — job completes, zero LLM calls, zero writes (source order verified: no-op return line 140 precedes `write_raw` line 149).
  - `prev == Some(other)` (drift) → after the raw overwrite, `vault::log::append_entry` writes `## [YYYY-MM-DD] drift | <sanitized title-or-url>` with bullets `- sha256 <old> -> <new>` and `- re-ingested <raw rel path>`, then extraction runs normally (pass 2 sees the existing page → merge path).
  - `prev == None` (first seen) → unchanged behavior, no drift entry.
  - `set_source_sha256` moved to **after** `run_extraction_passes` (documented in-code): a crash mid-extraction re-ingests on re-claim rather than silently no-opping (D-02).
- `vault/writer.rs`: `sanitize_for_nav` made `pub(crate)` (plan-preferred option) so the drift subject gets the same log-injection guard (T-02-05) as writer's own entries — no duplicated helper.

### Task 2 — end-to-end drift tests (`c203e91`)
- `tests/pipeline_drift.rs`: URL-keyed fetch stub (process-global `Mutex<HashMap<url, markdown>>`, sha256 computed with the real `sha2` crate) + four tests through the public `handle_job` entry:
  - `unchanged_source_is_noop` — run 1 scripts pass1+pass2 (call_count 2); run 2 uses a FRESH `MockProvider::new(vec![])` (any call would error the job): job completes, `call_count() == 0`, log.md/page/raw bytes byte-identical (Pitfall 1 token-burn proof).
  - `changed_source_drifts` — second run serves modified content: log.md gains `## [today] drift |` with `- sha256 <old_hex> -> <new_hex>` and `- re-ingested raw/<today>-drift-post.md`; raw file contains the new `[v2-marker]`; page reflects new content via merge; second run burns exactly 2 calls.
  - `drift_does_not_double_count_sources` — D-17 end-to-end: sub-threshold "Rust" stays at `count_entity_sources == 1` and page-less after re-ingesting the SAME url twice (INSERT OR IGNORE on source_id).
  - `first_seen_has_no_drift_entry` — fresh source: log.md has `create |` entries, no `drift |` line.

## Test Names Added

- `tests/pipeline_drift.rs::unchanged_source_is_noop`
- `tests/pipeline_drift.rs::changed_source_drifts`
- `tests/pipeline_drift.rs::drift_does_not_double_count_sources`
- `tests/pipeline_drift.rs::first_seen_has_no_drift_entry`

TDD evidence — RED: `cargo test -p saussunet-core --test pipeline_drift` → `test result: FAILED. 2 passed; 2 failed` (`unchanged_source_is_noop` panicked on handle_job error from the empty-script provider — the old code burned the LLM; `changed_source_drifts` failed "drift entry present"). GREEN after Task 1: `test result: ok. 4 passed; 0 failed`.

Final results: `--test pipeline_drift` → `4 passed; 0 failed`; `--test pipeline_extract` → `4 passed`; `--test pipeline_url` → `2 passed`; `--test queue` → `6 passed`; `bash scripts/check.sh` → `check: OK` (fmt, clippy `-D warnings`, zero-real-LLM guard, full test suite, axum-absence guard, release build).

Acceptance greps: `grep -n 'no-op\|drift' crates/saussunet-core/src/pipeline/mod.rs` → both branches present (no-op lines 133–140, drift lines 163–197); awk source-order check → `PASS: no-op return line 140 precedes write_raw line 149`. Threat-model greps intact: no `fs::write|write_atomic|File::create` in `pipeline/` (writes remain `write_raw` + `append_entry` + `write_page` funnel only).

## Task Commits

1. **Task 2 (tests written first, TDD RED):** `c203e91` (test)
2. **Task 1 (implementation, GREEN):** `559de2d` (feat)

**Plan metadata:** this commit (docs: complete plan)

## Deviations from Plan

**1. [Rule 3 - Pre-existing flake, out of scope] `unknown_kind_dead_letters_with_error_retained` / `provider_failure_routes_through_backoff_then_dead` intermittently fail with `SqliteError code 5 (database is locked)`**
- **Found during:** Task 1 acceptance — first `check.sh` run failed on `pipeline_url`, second on `queue`.
- **Issue:** immediate SQLITE_BUSY when the test's `fail_job` races the worker's `claim_jobs` deferred-transaction upgrade (fails in ~0.05s, well under the 5s busy_timeout — WAL upgrade deadlock, busy handler not invoked).
- **Fix:** none made — proven pre-existing: reproduced 2/4 runs on parent commit `8314150` (before any 03-04 change) via a throwaway worktree. Out of scope per deviation rules (pre-existing, unrelated to drift logic).
- **Verification:** third `check.sh` run → `check: OK`; standalone `--test pipeline_url` → 3/3 pass.
- **Committed in:** none.

**Total deviations:** 1 documented (pre-existing queue-test SQLite flake, not fixed — out of scope).
**Impact on plan:** none on PIPE-02; all acceptance criteria met. Flagged for a future hardening pass (retry-on-BUSY in `fail_job`/`claim_jobs` or `BEGIN IMMEDIATE` in the store).

## Decisions

- No-op gate placed between fetch and `write_raw` exactly per the plan decision — "skip" means skipping the extraction half; the fetch itself always runs (sha256 is only known post-fetch).
- Reused `writer::sanitize_for_nav` (made `pub(crate)`) instead of duplicating the 2-line helper — one log-injection guard for all log.md writers.
- `set_source_sha256` moved after `run_extraction_passes` (plan Task 1.4 exercised): crash-safety ordering documented in a code comment.
- Drift entry subject falls back to the submitted URL when the fetched title is blank; raw path in the entry is vault-relative (`raw/...`) matching provenance style.
- Test stub computes real sha256 (sha2 crate) so the log-entry digest assertions prove end-to-end digest flow rather than a fake marker.

## Lessons

- A fresh `MockProvider::new(vec![])` is a strict zero-LLM-call proof: any call errors and fails the job, so `handle_job` returning Ok + `call_count() == 0` is airtight.
- The queue tests' worker-vs-test `fail_job` race is a live flake source (immediate SQLITE_BUSY on WAL deferred-upgrade); any future executor hitting `database is locked` in `pipeline_url`/`queue` should treat it as this pre-existing flake, not a regression.
- `cargo test` flake triage: reproduce on the parent commit in a throwaway `git worktree` before assuming your change caused it.

## Issues Encountered

- `gsd-sdk query agent-skills gsd-executor` returned empty (known on this install) — followed `~/.hermes/profiles/devo/get-shit-done/workflows/execute-plan.md` + summary template per dispatch instructions.
- Two `check.sh` runs hit the pre-existing SQLite flake (see Deviations); third run green.

## Next Phase Readiness

- 03-05 (PDF upload): uploaded PDFs are drift-exempt by D-04 — the drift branch lives in `ingest_url` only; reuse `run_extraction_passes` after its raw write. The URL-keyed fetch-stub pattern in `pipeline_drift.rs` is reusable for multi-run tests.
- 03-06 (validator): unchanged — writes still funnel through `write_page`/`write_raw`/`append_entry`.
- Phase 4 (watcher): the no-op path is exactly what a poll loop needs — cheap fetch → sha compare → zero token spend when unchanged.
- Suggested follow-up (not in scope): retry-on-BUSY or `BEGIN IMMEDIATE` in `store` claim/fail paths to kill the queue-test flake.

---
*Phase: 03-extraction-pipeline*
*Completed: 2026-10-30*
