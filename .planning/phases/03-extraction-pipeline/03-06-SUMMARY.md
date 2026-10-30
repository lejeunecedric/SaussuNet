---
phase: 03-extraction-pipeline
plan: 06
subsystem: vault
tags: [validator, provenance, pipe-06, security, sqlite-migration]

requires:
  - phase: 03-extraction-pipeline (03-03)
    provides: [pipeline/pages.rs write site, write_page funnel, entity_registry, SCHEMA.md taxonomy]
provides:
  - "vault::validate::validate_generated_page — deterministic PIPE-06 gate (link whitelist + frontmatter + provenance + wiki-as-source rejection)"
  - "vault::writer::write_generated_page — the sole funnel for LLM-generated content"
  - "CoreError::Validation variant; pending_proposals reason 'validation_rejected' (migration 0008)"
affects: [03-05, 03-07, phase-5-review-queue]

tech-stack:
  added: []
  patterns:
    - "Validator-before-writer wrapper: validation rejects before any disk write; write_page stays the sole mutation entry (VLET-02)"
    - "All violations collected into one CoreError::Validation (joined list, rule names + offending targets only, never the body)"

key-files:
  created:
    - crates/saussunet-core/src/vault/validate.rs
    - crates/saussunet-core/tests/validator.rs
    - migrations/0008_proposal_reason_validation.sql
  modified:
    - crates/saussunet-core/src/vault/writer.rs
    - crates/saussunet-core/src/vault/mod.rs
    - crates/saussunet-core/src/error.rs
    - crates/saussunet-core/src/pipeline/pages.rs
    - crates/saussunet-core/src/store/proposals.rs
    - crates/saussunet-core/tests/pipeline_extract.rs
    - crates/saussunet-core/tests/pipeline_drift.rs

key-decisions:
  - "Wrapper shape exactly as planned: write_generated_page runs validate_generated_page then delegates to write_page — no bypass, same lock semantics; CLI write-page keeps plain write_page (human pages exempt from provenance rules)"
  - "extract_provenance scans ALL `^[...]` markers (broader than the plan's `^[raw/...]` regex) so markers pointing outside raw/ are deterministically REJECTED instead of silently ignored"
  - "Frontmatter parse/validate failures are folded into CoreError::Validation so the pipeline's rejection branch handles every validator rejection uniformly (one bad page never fails the job)"
  - "Migration 0008 skips the PRAGMA foreign_keys toggle: pending_proposals is a child table nothing references, and the PRAGMA is a no-op inside sqlx's migration transaction anyway"

patterns-established:
  - "PIPE-06 funnel: pipeline writes go only through write_generated_page → validate → write_page (grep-enforced: no direct write_page( in src/pipeline/)"
  - "Validation rejections recorded as pending_proposals rows (reason=validation_rejected) so Phase 5 review sees what never reached disk"

requirements-completed: [PIPE-06, PIPE-05]

duration: ~35min
completed: 2026-10-30
---

# Phase 3: Extraction Pipeline — Plan 06 Summary

**A deterministic Rust validator now stands between every LLM-composed page and the vault: link whitelist from index.md + entity registry, frontmatter schema, provenance presence with real raw/ targets, and a hard rejection of wiki-pages-cited-as-sources — a hallucinated page is recorded as a `validation_rejected` proposal and never touches disk.**

## Performance

- **Duration:** ~35 min
- **Started:** 2026-10-30 (Europe/Paris)
- **Completed:** 2026-10-30
- **Tasks:** 2
- **Files modified:** 11 (3 created, 8 modified)

## What Was Built

- `vault/validate.rs` (new): `link_whitelist(vault, store)` (index.md `- [[stem]] — title` stems ∪ registry canonical names ∪ registry page_path stems), `extract_wikilinks` (alias/anchor stripping, embed-skip — same contract as `count_outbound_wikilinks`), `extract_provenance` (all `^[...]` targets, deduped in order), and `validate_generated_page` — frontmatter gate (reuses `frontmatter::parse`/`validate_page`), sources non-empty + `raw/`-only + file-exists + Pitfall-4 wiki-layer rejection, ≥1 provenance marker with existing raw/ target, wikilinks ⊆ whitelist ∪ own stem. All violations joined into ONE `CoreError::Validation`. Strictly read-only (grep-proven).
- `error.rs`: new `CoreError::Validation(String)` (rule names + offending targets only — never the page body, D-03 log hygiene).
- `vault/writer.rs`: `write_generated_page(vault, store, rel_path, content)` — PIPE-06 funnel: validate → `write_page`. `write_page` remains the sole mutation entry; the wrapper delegates, never bypasses.
- `pipeline/pages.rs`: switched the single write site to `write_generated_page`; on `Err(CoreError::Validation)` → warn (rule names only), `insert_proposal(vault_id, rel_path, "validation_rejected", content)`, continue; job summary line `pipeline: {source_id} — N page(s) written, M validation-rejected`.
- `migrations/0008_proposal_reason_validation.sql`: CHECK-extension via table rebuild adding `validation_rejected` to `pending_proposals.reason` (columns/data preserved verbatim).
- Test fixtures updated to the new rules: pipeline_extract + pipeline_drift pass-2 fixtures now cite the real raw/ path and link index-seeded pages (Meta/JavaScript); contradiction test seeds a real `raw/old.md`.

## Test Names Added

`tests/validator.rs` (12): `valid_generated_page_accepted`, `dangling_wikilink_rejected_and_named`, `missing_provenance_marker_rejected`, `provenance_marker_to_nonexistent_raw_rejected`, `wiki_layer_source_rejected_pitfall4`, `nonexistent_raw_source_rejected`, `empty_sources_rejected`, `registry_entity_not_in_index_accepted`, `own_stem_link_accepted`, `provenance_marker_outside_raw_rejected`, `helpers_extract_targets_correctly`, `link_whitelist_covers_index_and_registry`.
`tests/pipeline_extract.rs` (1): `hallucinated_link_page_never_reaches_disk_and_is_recorded` (ROADMAP SC5: poisoned page absent from disk, `validation_rejected` proposal row, sibling page still written, job completes).

## TDD Evidence

- RED Task 1: `cargo test -p saussunet-core --test validator` → compile fail (`variant ... not found in CoreError` + `validate` module missing); after impl, 3 fixture failures (raw/ path under vault root) → GREEN `12 passed; 0 failed`.
- RED Task 2: `hallucinated_link_page_never_reaches_disk_and_is_recorded ... FAILED` (poisoned page reached disk via plain write_page) while the 4 existing extract + 4 drift tests stayed green → after wrapper + switch + migration 0008: `5 passed`, `4 passed`, `12 passed`.
- Full gate: `bash scripts/check.sh` → `check: OK` (fmt, clippy -D warnings, all suites incl. 97 lib tests, axum-absence guard, release build). No SQLITE_BUSY flake this run.

## Acceptance Criteria Verified

- `cargo test -p saussunet-core --test validator` → `test result: ok. 12 passed; 0 failed` (≥7 cases incl. full rejection matrix with offending-target substrings asserted).
- `grep -rn 'fs::write\|write_atomic\|File::create' crates/saussunet-core/src/vault/validate.rs` → EMPTY (exit 1).
- `grep -n 'write_generated_page' crates/saussunet-core/src/pipeline/pages.rs` → non-empty (line 170); `grep -rn 'write_page(' crates/saussunet-core/src/pipeline/` → EMPTY (PIPE-06 bypass proof).
- `grep -n 'write_page' crates/saussunet/src/main.rs` → 3 matches (CLI path untouched).
- `bash scripts/check.sh` → `check: OK`.

## Task Commits

1. **Task 1: validate.rs deterministic checks + error variant + rejection matrix** — `10caefd` (feat)
2. **Task 2: write_generated_page wrapper + pipeline switch + migration 0008 + SC5 test** — `493a013` (feat)

**Plan metadata:** this commit (docs(03-06): complete plan)

## Deviations from Plan

1. **`tests/pipeline_drift.rs` modified (not in plan files_modified):** its pass-2 fixtures cited the nonexistent `raw/stub.md`; once the pipeline routes through the validator those pages would be rejected. Updated the marker to the real raw/ path and seeded the index whitelist in setup — same treatment as pipeline_extract.rs. Verified: drift suite 4/4.
2. **`extract_provenance` scans all `^[...]` markers, not just `^[raw/...]`:** the plan's own rule ("every marker target starts `raw/`") requires non-raw markers to be visible so they can be rejected; a raw/-only regex would silently ignore `^[entities/foo.md]` self-citations. Covered by `provenance_marker_outside_raw_rejected`.
3. **Frontmatter failures folded into `CoreError::Validation`:** `validate_page` returns `Frontmatter` errors; the validator re-wraps them into the joined violations list so the pipeline's single rejection branch (Task 2) treats every validator rejection uniformly instead of dead-lettering the job.
4. **Migration 0008 omits `PRAGMA foreign_keys` toggles:** the table is a leaf (nothing references it) and the PRAGMA is a no-op inside sqlx's per-migration transaction; rebuild is data-preserving (verified by the proposal-inserting e2e test on a migrated DB).

**Impact on plan:** all deviations are consequences of the plan's own rules or SQLite mechanics; no scope creep.

## Decisions

- Whitelist matching is exact-string (case-sensitive) — deterministic by construction; canonical-name vs slug mismatches surface as loud rejections, not fuzzy matches.
- Rejection counting surfaces via a `tracing::info!` summary line at the end of `gate_and_write` (written + validation-rejected counts per source) rather than changing the function signature.
- Validation runs BEFORE acquiring the write lock (read-only reads of index.md/raw existence); `write_page` keeps its own lock semantics untouched.

## Lessons

- The vault root is `data/vaults/<id>`, NOT the configured `data_dir` — test fixtures writing raw/ evidence must use `vault.root()`, not the tempdir root (cost one RED cycle).
- Before wiring a new gate into an existing pipeline, grep the e2e fixtures for literals the gate will reject (`raw/stub.md`, unwhitelisted links) — both pipeline test files needed fixture updates the plan only anticipated for one of them.
- `cargo clippy --all-targets -- -D warnings` catches `while_let_loop` on `loop { let Some(..) else break }` scan idioms; write the `while let` form directly.

## Issues Encountered

None beyond the RED-cycle fixture path fix noted above. The known SQLITE_BUSY flake did not reproduce.

## Next Phase Readiness

- 03-05 (PDF upload) pages automatically pass through the same funnel — provenance must point at the raw record.
- Phase 5 review queue gains a fourth reason (`validation_rejected`) with full rejected content in `proposed_content`.

---
*Phase: 03-extraction-pipeline*
*Completed: 2026-10-30*
