---
status: complete
phase: 03-extraction-pipeline
source: 03-01-SUMMARY.md, 03-02-SUMMARY.md, 03-03-SUMMARY.md, 03-04-SUMMARY.md, 03-05-SUMMARY.md, 03-06-SUMMARY.md, 03-07-SUMMARY.md, 03-08-SUMMARY.md
started: 2026-10-30T22:44:44Z
updated: 2026-10-30T23:49:23Z
---

## Current Test

[testing complete]

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
result: pass
note: initially failed; diagnosed and fixed with 3 commits, then re-run live: serde.rs ingest wrote 8 pages (entities/serde.md, rust.md, cargo.md + 5 concepts) with valid provenance markers, gen_hash, index.md entries, and log.md create entries. Fixes: (a) 05c98d8 chat_template_kwargs enable_thinking=false — Halogen xhigh thinking took ~410-467s, past the 300s client timeout; (b) 62eeb11 prompt templates emitted doubled ^[raw/raw/...] markers the PIPE-06 validator rejected (13/13 pages; unit test masked it by accepting both forms); (c) e803e36 deterministic Related floor — small models ignore the >=2-wikilinks instruction, validator rejected every page.

## Summary

total: 9
passed: 8
issues: 0
pending: 0
skipped: 1
blocked: 0

## Gaps

- truth: "Ingesting a content-rich URL produces entity/concept pages via the live LLM path"
  status: resolved
  resolution: "Fixed in 05c98d8 (thinking disabled best-effort), 62eeb11 (single raw/ marker), e803e36 (wikilink Related floor); live re-run wrote 8 valid pages"
  reason: "User reported: local Halogen endpoint times out (>300s) on pass-1 prompts for substantial pages; repro with exact pass1_prompt never completed in 573s"
  severity: minor
  test: 9
  root_cause: "Not a hang: Halogen defaults to enable_thinking=true with reasoning_effort=xhigh (HALOGEN_MAX_TOKENS_DEFAULT=32768), so the model spends ~410s (~25k thinking tokens) before emitting the answer — streamed run of the exact 8637-byte pass-1 prompt completed at t=467s with valid JSON (finish=stop). The daemon's reqwest 300s total timeout (llm/openai_compat.rs) fires mid-thinking every time. Server logs corroborate: 'client disconnected mid-request (non-streaming), N token(s) generated and dropped'. Refuted: prompt pathology (same prompt returns valid JSON in 14.8s with chat_template_kwargs {enable_thinking:false}, 46s with reasoning_effort=low), server wedge, response_format interaction. Verdict: environment/model behavior + client sending no generation params — not a pipeline defect."
  artifacts:
    - path: "crates/saussunet-core/src/llm/openai_compat.rs"
      issue: "sends no generation parameters; 300s total timeout kills slow-thinking requests"
    - path: "~/.config/containers/systemd/user/halogen.container"
      issue: "server default xhigh thinking budget (HALOGEN_MAX_TOKENS_DEFAULT=32768) shared by all clients"
  missing:
    - "Send chat_template_kwargs {enable_thinking:false} (or reasoning_effort low) best-effort, mirroring the existing response_format fallback"
    - "Optional explicit max_tokens paired with finish_reason==length handling"
    - "Or streaming with idle timeout instead of 300s total timeout"
  debug_session: "/home/c/.hermes/profiles/devo/cache/delegation/live/deleg_9da2859d/task-0.log"
