---
status: partial
phase: 03-extraction-pipeline
source: 03-01-SUMMARY.md, 03-02-SUMMARY.md, 03-03-SUMMARY.md, 03-04-SUMMARY.md, 03-05-SUMMARY.md, 03-06-SUMMARY.md, 03-07-SUMMARY.md, 03-08-SUMMARY.md
started: 2026-10-30T22:44:44Z
updated: 2026-10-30T23:59:00Z
---

## Current Test

number: 9
name: Live page generation (pass-2 merge path writes a real page)
expected: |
  Ingesting a content-rich URL produces entity/concept pages under entities/ or concepts/, an index.md entry, and a log.md create entry.
awaiting: user response

## Tests

### 1. Cold start — daemon boots on a fresh vault
expected: `init` scaffolds vault (SCHEMA.md, index.md, log.md); daemon serves /health; SQLite DB with 8 migrations
result: pass
note: UAT vault /tmp/ss-uat; health `ok`; 8 migrations applied; scaffold present.

### 2. URL ingest end-to-end (live LLM)
expected: POST /ingest with a URL queues a job that fetches, converts to markdown, writes raw/ with frontmatter (source_url, ingested, sha256), runs extraction, completes
result: pass
note: job 3 (https://example.com/) → done; raw/2026-10-30-example-domain.md with full frontmatter; zero LLM errors.

### 3. sha256 no-op on unchanged re-submit
expected: re-submitting the same URL completes as no-op with zero LLM calls and zero new writes
result: pass
note: job 4 (same URL) → done in <15s (vs minutes for LLM path); raw/ file count unchanged; matches D-08.

### 4. PDF upload ingest
expected: POST /ingest/upload accepts a real PDF, stores it under raw/papers/, worker extracts via pdftotext, job completes
result: pass
note: job 5 → done; raw/papers/2026-10-30-sample.pdf stored; response carried raw_path.

### 5. SSRF guard on ingest
expected: loopback and cloud-metadata URLs refused at submit time with 400
result: pass
note: http://127.0.0.1:8731/... → 400; http://169.254.169.254/... → 400 "private/loopback address".

### 6. Upload guards (extension/content)
expected: non-PDF content with .pdf filename rejected with 400
result: pass
note: /etc/hostname uploaded as evil.pdf → 400 (%PDF- magic check).

### 7. Retry + dead-letter lifecycle
expected: a repeatedly failing job backs off through attempts and lands in dead with the last error retained
result: pass
note: job 2 (rich page, LLM timeout) → dead after 4 attempts, error retained verbatim.

### 8. Source drift (changed content, same URL)
expected: same URL with changed sha → log.md drift entry + re-extraction
result: skipped
reason: needs a public URL whose content changes between fetches; none reliable available. Covered by automated tests (tests/pipeline_drift.rs, 4/4 green per 03-VALIDATION.md).

### 9. Live page generation (pass-2 merge path writes a real page)
expected: a content-rich URL produces pages under entities/ or concepts/ + index.md + log.md create entries
result: issue
reported: "Local Halogen model (halogen-qwen3.8-flash-next) hangs >300s on pass-1 prompts for substantial pages (~8KB+ markdown): job 2 (Rust blog, dead after 4 attempts), job 7 (serde.rs, failing retries). A standalone repro with the exact pass1_prompt never completed in 573s (killed). Small pages work (example.com done; cargo doc page done but model returned no central candidates so gating correctly wrote zero pages). Page-write path itself is covered by automated tests (03-06 validator, phase3_acceptance sc1-sc5)."
severity: minor

## Summary

total: 9
passed: 7
issues: 1
pending: 0
skipped: 1
blocked: 0

## Gaps

- truth: "Ingesting a content-rich URL produces entity/concept pages via the live LLM path"
  status: failed
  reason: "User reported: local Halogen endpoint times out (>300s) on pass-1 prompts for substantial pages; repro with exact pass1_prompt never completed in 573s"
  severity: minor
  test: 9
  root_cause: ""     # Filled by diagnosis
  artifacts: []      # Filled by diagnosis
  missing: []        # Filled by diagnosis
  debug_session: ""  # Filled by diagnosis
