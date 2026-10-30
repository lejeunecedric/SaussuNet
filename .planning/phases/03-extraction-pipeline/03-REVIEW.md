---
status: issues_found
phase: 03-extraction-pipeline
date: 2026-10-30
---

# Phase 3 code review — extraction pipeline

Range: `88d8791..082f338` (32 commits, source only). Reviewed: SSRF guard, write-path funnel, store SQL, multipart guards, pdftotext subprocess, log/error hygiene, sha256 no-op/drift flow, config plumbing.

## CRITICAL

None.

## HIGH

None.

## MEDIUM

1. **`crates/saussunet-core/src/pipeline/fetch.rs:207` — `max_fetch_bytes` is configured but never enforced; fetch body is unbounded.**
   `[pipeline].max_fetch_bytes` (default 5 MiB) is parsed in `config.rs:111` and plumbed to `RuntimeConfig.pipeline_max_fetch_bytes` (`config.rs:251`), but no code reads it. `fetch_url` calls `resp.text()`, buffering the ENTIRE response with no cap — the only bound is the 30 s timeout, which on a fast line still allows multi-GB buffering → OOM of the daemon from one hostile/misconfigured public host. The upload route correctly enforces its cap two ways (route `DefaultBodyLimit` + running per-chunk cap in `http.rs:146-159`); the fetch path has neither.
   **Fix:** check `Content-Length` against the cap, then stream with a running byte cap (same pattern as `http.rs` upload), threading `max_fetch_bytes` into the fetcher.

## LOW

1. **`crates/saussunet-core/src/pipeline/mod.rs:428` — `handle_job` uses `DEFAULT_MIN_SOURCES` instead of `RuntimeConfig.pipeline_min_sources`** (known, documented seam at `mod.rs:356-360`). A user-set `[pipeline].min_sources` silently has no effect on the queue path. One-line plumbing once config reaches the handler.

2. **`crates/saussunet-core/src/pipeline/mod.rs:319-343` — duplicate drift entries on retry.** The `log.md` drift entry is appended BEFORE extraction, while `set_source_sha256` lands only after extraction succeeds (`mod.rs:350-352`). If extraction fails and the queue re-claims the job, `prev` is still the old sha → the same drift entry is appended again on every retry. Suggested fix: append the drift entry after extraction succeeds (alongside the sha update), or dedupe same-day/same-source entries.

3. **`crates/saussunet-core/src/pipeline/mod.rs:251-253` — `ingest_pdf` records `sha256` BEFORE extraction, contradicting the documented post-extraction rule** used by `ingest_url` (`mod.rs:346-352`). Currently benign (the PDF path has no no-op gate), but if a no-op gate is ever added to `ingest_pdf`, a job that crashed mid-extraction would silently skip re-extraction on re-claim — exactly the D-02 failure the 03-04 move was made to prevent. Suggested fix: move the `set_source_sha256` call after `run_extraction_passes`, or comment why PDF differs.

4. **`scripts/check.sh:33` — the PIPE-06 write-funnel grep is bypassable.** Pattern `fs::write|File::create` misses `OpenOptions::new().write(true)`, `fs::copy`, `fs::rename`, and `File::open`-based writes; and the `grep -v '//'` filter drops any violating line that merely contains `//` (trailing comment, URL in a string literal). The funnel itself is clean today (verified: the only core-src writers are `vault/atomic.rs` + `vault/raw.rs`; the pipeline's only page write is `write_generated_page` at `pages.rs:170`). Suggested fix: broaden the pattern and drop the `//` filter in favor of a proper exclusion.

## INFO

- **DNS-rebinding TOCTOU** in the SSRF guard is a documented, accepted v1 residual risk (`fetch.rs:10-14`); guard itself is solid: scheme allowlist, private/loopback/link-local/CGNAT/mapped-v6 ranges, ANY-DNS-answer refusal, manual redirect loop re-guarding every hop (`fetch.rs:170-199`), userinfo scrubbed before Display/logs.
- **sha256 no-op race:** two concurrent jobs for the same `source_id` both pass the no-op check and duplicate LLM work; writes are idempotent (same-day overwrite, `INSERT OR IGNORE` source sets) and default `max_concurrency = 1`, so no data loss — acceptable for v1.
- **`set_fetcher_override` (`pipeline/mod.rs:88`)** is a process-wide `OnceLock` test seam, `doc(hidden)`, never called by production code — fine, but it is a permanent production-visible injection point.
- **`http.rs:73`** returns serde parse error text (may echo a body fragment) to the caller; endpoint is loopback-bound with a 64 KiB cap — negligible.
- **`extract_provenance` (`validate.rs:87`)** also matches `^[` inside code spans → possible false-positive rejections; errs on the safe side.
- **`pdftotext` subprocess (`extract.rs:28-75`)**: no shell, fixed flags, path-only args, 60 s timeout, stderr tail-only in errors, LOUD missing-binary error — clean. `SAUSSUNET_PDFTOTEXT_PATH` is an operator-env override, documented.
- **Store SQL:** all new APIs (`add_entity_source`, `count_entity_sources`, `merge_aliases`, `set/get_source_sha256`) are fully parameterized — no injection surface.
- **Multipart guards:** per-route `DefaultBodyLimit` (64 KiB JSON / `max_upload_bytes` upload), running per-chunk cap, `%PDF-` magic + `.pdf` extension + `application/pdf` content-type, filename folded to `[a-z0-9-]` via `sanitize_component` before any path use — clean.
- **Log hygiene:** prompts clipped to 400 chars at debug, filenames/bodies never logged, `CoreError` variants carry pre-sanitized strings, `sanitize_for_nav` guards the drift log subject — clean (backed by `security_hygiene.rs` gates).

## Verified intent-vs-code

- PIPE-06 funnel holds: `write_generated_page` (`writer.rs:298-308`) validates then delegates to `write_page`; no pipeline write bypasses it; CI grep enforces (with the LOW-4 caveat).
- PIPE-02 no-op: zero LLM calls / zero writes on digest match (`mod.rs:290-294`); sha stored post-extraction for URLs.
- PIPE-03/04/05: threshold gate, resolve-before-create, alias merge, source-set recording incl. sub-threshold — implemented as planned.
- Migration 0007 backfill (`sha256 = token`) and 0008 CHECK rebuild are correct.
