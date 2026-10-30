---
phase: 03-extraction-pipeline
plan: 01
subsystem: pipeline
one_liner: "A queued ingest_url job now fetches (SSRF-guarded), extracts article Markdown, and lands raw/YYYY-MM-DD-slug.md with source_url/ingested/sha256 frontmatter + sync_state sha256 — the phase's walking skeleton."
tags: [reqwest, readability, htmd, ssrf, sha256, tokio]

requires:
  - phase: 02-llm-provider-durable-job-queue
    provides: [durable claim/heartbeat/fencing queue, MockProvider, spawn_workers]
  - phase: 01-workspace-foundations-vault-writer
    provides: [Vault write_lock, atomic::write_atomic, sync_state store]
provides:
  - "pipeline::fetch — assert_public_http_url SSRF gate + manual-redirect fetch_url"
  - "pipeline::extract — readability+htmd HTML→Markdown with whole-document fallback"
  - "vault::raw::write_raw — sole raw/ writer (lock-respecting, atomic, D-07 naming)"
  - "pipeline::handle_job kind dispatch wired into queue::process_job (ingest_url v1)"
  - "Fetcher trait + set_fetcher_override test seam (zero-network worker tests)"
affects: [03-02, 03-03, 03-05, 03-06]

tech-stack:
  added: [url 2, readability 0.3, htmd 0.1, tokio net feature]
  patterns:
    - "kind-dispatch job handlers (queue → pipeline::handle_job)"
    - "test-only process-global fetcher seam keyed behaviour off payload URL"

key-files:
  created:
    - crates/saussunet-core/src/pipeline/mod.rs
    - crates/saussunet-core/src/pipeline/fetch.rs
    - crates/saussunet-core/src/pipeline/extract.rs
    - crates/saussunet-core/src/vault/raw.rs
    - crates/saussunet-core/tests/pipeline_url.rs
  modified:
    - Cargo.toml
    - crates/saussunet-core/Cargo.toml
    - crates/saussunet-core/src/error.rs
    - crates/saussunet-core/src/lib.rs
    - crates/saussunet-core/src/vault/mod.rs
    - crates/saussunet-core/src/queue/mod.rs
    - crates/saussunet/src/main.rs
    - crates/saussunet-core/tests/queue.rs

key-decisions:
  - "assert_public_http_url is async (DNS check via tokio::net::lookup_host)"
  - "sha256 stored in sync_state.token column as documented placeholder until 03-03 migration 0007"
  - "Fetcher trait + process-wide set_fetcher_override as the test-only injection seam"
  - "ingest_pdf kind constant registered but handler fails loudly until 03-05"

patterns-established:
  - "SSRF guard: scheme allowlist + private-range rejection incl. 169.254/16 + 100.64/10, manual redirect loop re-guarding every hop"
  - "raw/ writes funnel through vault::raw::write_raw (write_lock + atomic::write_atomic only)"

requirements-completed: [PIPE-01]

duration: ~50min
completed: 2026-10-30
---

# Phase 3 Plan 01: URL → raw/ walking slice Summary

**A queued `ingest_url` job fetches through an SSRF guard, converts HTML to article Markdown, and lands durable `raw/YYYY-MM-DD-slug.md` evidence with D-07 frontmatter and a sync_state sha256 — no LLM yet, zero real network in tests.**

## Performance

- **Duration:** ~50 min
- **Completed:** 2026-10-30T11:55Z
- **Tasks:** 2 (both TDD: RED observed on write_raw stubs, then GREEN)
- **Files created/modified:** 14

## What Was Built

### Task 1 — fetch + extract + raw writer primitives (`e87ec01`)
- `pipeline/fetch.rs`: `assert_public_http_url` (scheme allowlist http/https; rejects IPv4/IPv6 literals AND every DNS answer in 127/8, ::1, 10/8, 172.16/12, 192.168/16, 169.254/16, fe80::/10, 100.64/10, 0.0.0.0/:: incl. IPv4-mapped v6; strips userinfo from the returned URL and from every error Display). `fetch_url`: manual redirect loop (max 5, guard re-run on every hop), 30 s timeout, clean UA, body → `extract::html_to_markdown`, sha256 of the markdown in `FetchResult`. DNS-rebinding TOCTOU documented as accepted v1 residual risk in the module doc.
- `pipeline/extract.rs`: `html_to_markdown` via `readability::extractor::extract` + `htmd::convert`, whole-document htmd fallback when readability finds no article.
- `vault/raw.rs`: `write_raw` — the sanctioned raw/ writer: slug/ext ASCII-folded to `[a-z0-9-]`, subdir traversal hard-refused, containment asserted against `raw/`, `vault.write_lock` held, bytes landed via `atomic::write_atomic` only. Same-day re-ingest = atomic overwrite (allowed).
- `error.rs`: `CoreError::Fetch` / `CoreError::BlockedUrl` (Display carries pre-sanitized URLs only).
- Workspace deps: `url 2`, `readability 0.3`, `htmd 0.1`; tokio gained the `net` feature.

### Task 2 — job kinds, kind dispatch, ingest_url handler v1 (`e98a278`)
- `pipeline/mod.rs`: `JOB_KIND_INGEST_URL`/`JOB_KIND_INGEST_PDF`, `IngestUrlPayload` (tolerant serde), `handle_job` kind dispatch (unknown kind → `Queue("unknown job kind: …")` → dead-letters via fail_job ladder), `ingest_url` v1 (fetch → `write_raw` → `upsert_sync_state` sha256 in the `token` column — documented placeholder until 03-03 migration 0007), `run_extraction_passes` no-op seam marked `TODO(03-03)`.
- `Fetcher` trait + `HttpFetcher` (guard → reqwest) + `#[doc(hidden)] set_fetcher_override` test seam.
- `queue/mod.rs`: `spawn_workers(store, provider, vault: Arc<Vault>, vault_id, cfg)`; `process_job` heartbeat select now awaits `pipeline::handle_job`; generic provider prompt and its stale `generate_json` doc comment removed; WR-01 fencing (claim_token on complete/fail/touch) untouched; threat-model comment updated (payload now drives a URL fetch; SSRF guard is the mitigation).
- `main.rs`: call site updated (`Arc::new(vault)`).
- `tests/pipeline_url.rs`: e2e insert_job → worker → raw file on disk + sync_state sha256; unknown-kind dead-letter with error retained.
- `tests/queue.rs`: adapted to the new dispatch (see Deviations).

## Test Names Added

- `pipeline::fetch::tests::ip_classification`
- `pipeline::fetch::tests::ssrf_guard_rejects_private_targets` (file://, 127.0.0.1, 169.254.169.254, 10.0.0.5, [::1], gopher://, userinfo+loopback)
- `pipeline::fetch::tests::ssrf_guard_strips_userinfo_from_returned_url`
- `pipeline::fetch::tests::ssrf_guard_accepts_public_https`
- `pipeline::extract::tests::extracts_article_body`
- `pipeline::extract::tests::thin_page_falls_back_to_whole_document`
- `vault::raw::tests::write_raw_lands_dated_file_with_frontmatter`
- `vault::raw::tests::write_raw_refuses_escapes_and_lands_in_subdir`
- `vault::raw::tests::slug_sanitisation`
- `tests/pipeline_url.rs::ingest_url_job_lands_raw_file_and_sync_state`
- `tests/pipeline_url.rs::unknown_kind_dead_letters_with_error_retained`

Final results: lib `82 passed; 0 failed`; `pipeline_url` `2 passed`; `queue` `6 passed`; all Phase 1/2 suites green; `bash scripts/check.sh` → `check: OK` (fmt, clippy -D warnings, zero-real-LLM guard, axum-absence guard, release build). Acceptance greps: `grep -n generate_json queue/mod.rs` → no match; `grep -c claim_token queue/mod.rs` → 7; `grep axum crates/saussunet-core/Cargo.toml` → empty; `grep -c write_atomic vault/raw.rs` → 3; no `fs::write`/`File::create` in `vault/` outside `atomic.rs`. `saussunet init-vault` boots unchanged.

## Task Commits

1. **Task 1: fetch + extract + raw writer primitives** — `e87ec01` (feat)
2. **Task 2: job kinds, kind dispatch, ingest_url handler v1** — `e98a278` (feat+test)

## Deviations from Plan

1. **`assert_public_http_url` is `async`** — the plan's signature was `pub fn` but the mandated `tokio::net::lookup_host` DNS check forces async. Tests are `#[tokio::test]`.
2. **readability 0.3 API** — 0.3.0 exposes `readability::extractor::extract(&mut reader, &Url) -> Product` (no `extract_content` top-level fn as later versions have); extract.rs adapted.
3. **`tests/queue.rs` rewritten (not in plan's files_modified)** — removing the generic provider prompt (required by acceptance criteria) invalidated the Phase 2 tests' MockProvider SlowOk/Hang scripts. All six tests were preserved 1:1 with the same guarantees; the slow/hang/fail behaviour moved to a URL-keyed stub behind the new `Fetcher` seam. Zero-real-LLM guard still passes (MockProvider only).
4. **Test seam shape** — plan allowed "Fetcher trait OR fetch_override seam"; implemented Fetcher trait + process-global `set_fetcher_override` (first call wins) so the worker path is testable without widening `spawn_workers` beyond the plan's signature. Stub behaviour is keyed off the payload URL to stay race-free under parallel tests.
5. **`htmd = "0.1"` pinned literally** (0.5.6 exists) per the plan's explicit version; `readability = "0.3"` is latest stable as instructed.
6. **tokio workspace feature `net` added** — required by `lookup_host`.

**Impact on plan:** all deviations were forced by the plan's own constraints (async DNS check, prompt removal); no scope creep.

## Decisions

- sha256 rides the `sync_state.token` column as a documented placeholder; 03-03 migration 0007 moves it to a dedicated column (comment in `pipeline/mod.rs`).
- `ingest_pdf` kind is registered but its handler fails loudly until 03-05 — dead-lettering beats silently accepting an unimplemented kind.
- raw slug source = article title, falling back to the URL host when the title is empty.
- Mixed A/AAAA records: guard refuses a hostname if ANY resolved answer is private (no happy-eyeballs smuggling).
- subdir argument to `write_raw` is hard-refused on any `.`/separator (structural names only); slug is ASCII-folded instead.

## Lessons

- `readability` 0.3's API differs from 0.4+ (`extractor::extract` reader-based) — check the pinned version's docs before writing the call site.
- `static Mutex<HashMap<..>>` needs `LazyLock` (HashMap::new isn't const).
- A process-global test override must be stateless-per-test or keyed by input (URL) — plain global flags race between parallel `#[tokio::test]`s in one binary.
- The queue tests' behavioural knobs (slow/hang/fail) belong behind the seam the pipeline actually calls; keeping them on the unused provider would have made the tests vacuous.

## Issues Encountered

- `gsd-sdk query agent-skills gsd-executor` returned empty on this install (known) — followed `~/.hermes/profiles/devo/get-shit-done/workflows/execute-plan.md` + summary template instead, per dispatch instructions.

## Next Phase Readiness

- 03-02 (HTTP `POST /ingest` in the binary) can insert `JOB_KIND_INGEST_URL` jobs directly; the queue drains them unchanged.
- 03-03 fills `run_extraction_passes` (currently a no-op seam) and migration 0007 moves the sha256 out of the `token` column.
- 03-05 replaces the loud `ingest_pdf` refusal.

---
*Phase: 03-extraction-pipeline*
*Completed: 2026-10-30*
