---
phase: 03-extraction-pipeline
plan: 05
subsystem: api
tags: [pdf, pdftotext, axum-multipart, subprocess, upload-security, poppler]

# Dependency graph
requires:
  - phase: 03-extraction-pipeline/03-01
    provides: vault::raw::write_raw, handle_job kind dispatch, IngestPdfPayload seam
  - phase: 03-extraction-pipeline/03-02
    provides: axum router + [pipeline].max_upload_bytes config
  - phase: 03-extraction-pipeline/03-03
    provides: run_extraction_passes (shared two-pass LLM extraction)
  - phase: 03-extraction-pipeline/03-04
    provides: sync_state sha256 plumbing (set_source_sha256)
provides:
  - "pipeline::submit_pdf — guarded PDF intake (size cap, %PDF- magic, .pdf ext, slug whitelist) -> raw/papers/YYYY-MM-DD-slug.pdf byte-exact copy + ingest_pdf job"
  - "pipeline::handle_job kind=ingest_pdf — pdftotext subprocess -> derived raw .md (source_url = pdf copy) -> shared run_extraction_passes"
  - "extract::pdf_to_text — tokio subprocess wrapper, 60s timeout, SAUSSUNET_PDFTOTEXT_PATH override, MissingBinary LOUD failure (D-06)"
  - "vault::raw::write_raw_binary — frontmatter-free byte-exact binary raw record"
  - "POST {base}/ingest/upload — multipart route, per-route DefaultBodyLimit, 400/413 guard mapping"
affects: [phase-4-source-watching, phase-5-ui, http-surface, pipeline]

# Tech tracking
tech-stack:
  added: [pdftotext (poppler-utils subprocess), axum multipart feature, tokio process feature]
  patterns:
    - "LOUD subprocess failure: CoreError::MissingBinary Display carries the actionable install hint straight into jobs.error"
    - "Per-route DefaultBodyLimit: upload route gets max_upload_bytes, JSON ingest keeps 64 KiB"
    - "Binary raw records: write_raw_binary keeps the immutable copy byte-exact; sha256/source_url live on the derived .md + sync_state"

key-files:
  created:
    - crates/saussunet-core/tests/pipeline_pdf.rs
    - crates/saussunet-core/tests/fixtures/sample.pdf
    - crates/saussunet/tests/http_upload.rs
  modified:
    - Cargo.toml
    - crates/saussunet-core/src/pipeline/mod.rs
    - crates/saussunet-core/src/pipeline/extract.rs
    - crates/saussunet-core/src/pipeline/fetch.rs
    - crates/saussunet-core/src/error.rs
    - crates/saussunet-core/src/vault/raw.rs
    - crates/saussunet/src/http.rs
    - crates/saussunet/src/main.rs
    - crates/saussunet/tests/http_ingest.rs

key-decisions:
  - "submit_pdf returns (job_id, raw_rel) instead of bare i64 so the endpoint can echo raw_path in the 202 body (task 2 response contract); the payload still carries raw_path/source_id/filename per the plan schema"
  - "sha256 is taken from the COPY inside the worker (D-04), not at submit — payload schema stays {raw_path, source_id, filename}"
  - "write_raw_binary (no frontmatter) added instead of bending write_raw: a binary PDF copy must stay byte-exact (immutable record)"
  - "Missing-binary test drives the real queue (spawn_workers) and asserts jobs.error LIKE '%poppler-utils%'; env-var tests serialized behind an async mutex to avoid cross-test races on SAUSSUNET_PDFTOTEXT_PATH"
  - "Multipart guard errors map via MultipartError::status() so the route-level DefaultBodyLimit trips surface as 413, not 400"

requirements-completed: [PIPE-01]

# Metrics
duration: 25min
completed: 2026-10-30
---

# Phase 3 Plan 5: PDF slice — upload → raw/ → pdftotext → pages Summary

**PDF uploads now flow end-to-end: multipart POST copies the byte-exact PDF into raw/papers/ at submit time, the ingest_pdf worker extracts text with a pdftotext subprocess (LOUD poppler-utils hint in jobs.error when missing) and runs the SAME two-pass extraction as URLs — pages land with provenance to the PDF-derived raw .md.**

## Performance

- **Duration:** ~25 min
- **Started:** 2026-10-30T13:13Z (approx)
- **Completed:** 2026-10-30T13:37Z
- **Tasks:** 2
- **Files modified:** 13 (incl. Cargo.lock)

## Task Commits

1. **Task 1: core submit_pdf + ingest_pdf handler + pdftotext** — `e6348de` (feat)
2. **Task 2: multipart upload route + e2e HTTP tests** — `c14ccf6` (feat)
3. **Cargo.lock for new features** — `031d050` (fix)

## Files Created/Modified

- `crates/saussunet-core/src/pipeline/mod.rs` — `submit_pdf` (guards + copy + job insert), `ingest_pdf` handler (raw-guarded read → pdf_to_text → derived raw .md → set_source_sha256 → run_extraction_passes), `IngestPdfPayload`, kind registration
- `crates/saussunet-core/src/pipeline/extract.rs` — `pdf_to_text`: tokio subprocess `-q -layout <pdf> -`, 60s timeout, NotFound → MissingBinary, truncated stderr tail on non-zero exit
- `crates/saussunet-core/src/error.rs` — `CoreError::MissingBinary { binary, hint }`
- `crates/saussunet-core/src/vault/raw.rs` — `write_raw_binary` + shared `raw_target` path gauntlet; `sanitize_component` now pub(crate)
- `crates/saussunet-core/src/pipeline/fetch.rs` — `sha256_hex` pub(crate) (reused, no duplicate)
- `crates/saussunet/src/http.rs` — `POST {base}/ingest/upload` multipart handler; AppState gains `vault` + `max_upload_bytes`; per-route DefaultBodyLimit
- `crates/saussunet/src/main.rs` — AppState wiring
- `crates/saussunet-core/tests/fixtures/sample.pdf` — 605-byte hand-built valid PDF ("Saussure linguistics signifier"), verified with pdftotext
- `crates/saussunet-core/tests/pipeline_pdf.rs` — 7 tests
- `crates/saussunet/tests/http_upload.rs` — 5 router tests
- `Cargo.toml` — tokio `process` feature, axum `multipart` feature

## Tests

pipeline_pdf (7): `submit_pdf_rejects_oversized_upload`, `submit_pdf_rejects_non_pdf_bytes`, `submit_pdf_rejects_non_pdf_extension`, `submit_pdf_sanitizes_traversal_filename`, `submit_pdf_happy_path_returns_job_id_and_immutable_copy`, `ingest_pdf_e2e_extracts_pages_with_provenance` (self-skips loud if poppler absent), `missing_pdftotext_fails_loud_with_install_hint` (queue-driven; jobs.error contains "poppler-utils").

http_upload (5): `valid_upload_returns_202_job_row_and_pdf_on_disk_at_submit_time`, `wrong_content_type_is_400_and_lands_nothing`, `non_pdf_magic_bytes_are_400_and_land_nothing`, `oversized_upload_is_413_and_lands_nothing`, `traversal_filename_is_stored_sanitized_inside_raw_papers`.

Test-result lines:
- `cargo test -p saussunet-core --test pipeline_pdf` → `test result: ok. 7 passed; 0 failed`
- `cargo test -p saussunet --test http_upload` → `test result: ok. 5 passed; 0 failed`
- `cargo test -p saussunet --test http_ingest` → `test result: ok. 6 passed; 0 failed`
- `bash scripts/check.sh` → `check: OK` (fmt, clippy -D warnings, full suite, axum-absence guard, release build)
- Manual: `curl -s -F 'file=@sample.pdf' localhost:8080/saussunet/ingest/upload` → `{"job_id":1,"raw_path":"raw/papers/2026-10-30-sample.pdf"}`; PDF present under `data/vaults/1/raw/papers/`
- Greps: `grep -rn 'Command::new' crates/saussunet-core/src/pipeline/` → only `tokio::process::Command::new(&binary)` in extract.rs (no `sh -c` anywhere in core src); `grep -c axum crates/saussunet-core/Cargo.toml` → 0; `grep 'write_page(' crates/saussunet-core/src/pipeline/` → empty (03-06 funnel intact)

TDD evidence: pipeline_pdf.rs and http_upload.rs were written first and observed RED (E0425 `submit_pdf` not found; E0560 AppState missing fields / no upload route), then GREEN after implementation.

## Decisions Made

- `submit_pdf` returns `(job_id, raw_rel)` — the plan's frontmatter said `Result<i64>` but task 2's 202 body requires `raw_path`; the tuple satisfies both without re-reading the job row.
- sha256 computed from the copy inside the worker (D-04 wording), keeping the payload schema exactly `{raw_path, source_id, filename}`.
- `write_raw_binary` added rather than making write_raw's frontmatter optional — existing callers untouched; binary copy stays byte-exact.
- Env-var-sensitive tests (SAUSSUNET_PDFTOTEXT_PATH) serialized behind a tokio async mutex inside the test binary to avoid parallel-test races.

## Deviations from Plan

**1. [Rule 1 - Minor interface refinement] submit_pdf returns (job_id, raw_rel) not bare i64**
- **Found during:** Task 1/2 boundary — task 2's 202 response includes raw_path
- **Fix:** tuple return; job payload still carries raw_path per plan schema
- **Verification:** both test suites assert on the tuple + payload; 202 body carries both fields

**2. [Rule 2 - Missing Critical] write_raw_binary (no-frontmatter raw writer)**
- **Found during:** Task 1 step 3 — write_raw always prepends YAML frontmatter, which would corrupt the byte-exact PDF copy (D-04)
- **Fix:** new `write_raw_binary` sharing the same path gauntlet + lock + atomic write
- **Verification:** happy-path test asserts `on_disk == PDF` byte-exact

**3. [Rule 1 - Wiring beyond files_modified] AppState gained `vault` + `max_upload_bytes`; main.rs + http_ingest.rs updated**
- **Found during:** Task 2 — submit_pdf needs the Vault handle and the cap; AppState had neither
- **Verification:** http_ingest (6) + http_upload (5) pass; daemon boots and serves curl smoke test

**4. [Rule 1 - Correctness] Multipart guard errors mapped via MultipartError::status()**
- **Found during:** Task 2 acceptance — route-level DefaultBodyLimit trips inside next_field() and surfaced as 400 instead of the required 413
- **Verification:** `oversized_upload_is_413_and_lands_nothing` passes

**Total deviations:** 4 auto-fixed (1 interface refinement, 1 missing critical, 2 correctness/wiring). **Impact on plan:** all required to satisfy the plan's own acceptance criteria; no scope creep.

## Issues Encountered

- axum 0.8 `Field::bytes()` consumes the field — used `field.chunk()` in a loop for the running size cap instead.
- Known pre-existing flake (pipeline_url unknown_kind / queue provider_failure, SQLITE_BUSY) did NOT recur; full suite green first try.

## Next Phase Readiness

- PIPE-01 now TRUE for both source types (URL + PDF → raw/ + pages); ROADMAP SC1 covered.
- CI requirement to document: extraction e2e needs poppler-utils installed (test self-skips with loud eprintln otherwise).
- 03-07/03-08: any new pipeline write path must keep using vault::writer::write_generated_page (grep 'write_page(' src/pipeline/ stays empty — verified this plan).

## User Setup Required

None — no external service configuration required (pdftotext is a system package, hinted LOUD at runtime if missing).

---
*Phase: 03-extraction-pipeline*
*Completed: 2026-10-30*
