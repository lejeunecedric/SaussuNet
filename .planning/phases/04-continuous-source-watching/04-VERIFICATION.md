---
phase: 4
date: 2026-11-02
status: human_needed
score: 5/5
verified_commits: a07a823
verification_method: executed (cargo test suites + scripts/check.sh + code spot-checks)
---

# Phase 4 — Verification Report (Continuous Source Watching)

**Goal under test:** The vault compounds on its own: a configured WebDAV folder is polled continuously, and watcher state survives restarts without re-ingesting unchanged files.

**Verdict:** All three success criteria are proven TRUE by executed automated tests. All 16 critical-case tests (T1–T16) from `04-VALIDATION.md` pass. Full gate `bash scripts/check.sh` → `check: OK`. One human-verification item remains (real LAN WebDAV UAT — inherently manual per 04-VALIDATION.md), hence `status: human_needed`.

---

## Runs performed (all on commit a07a823, 2026-11-02)

| Command | Result |
|---------|--------|
| `cargo test -p saussunet-core` (full) | **282 passed; 0 failed; 1 ignored** (ignored = `llm_live`, gated real-LLM test, pre-existing) |
| `cargo test -p saussunet-core config` | 31 passed; 0 failed |
| `cargo test -p saussunet-core store` | 12 passed; 0 failed |
| `cargo test -p saussunet-core watch` | 27 passed; 0 failed |
| `cargo test -p saussunet-core --test watch_webdav` | **20 passed; 0 failed; finished in 0.33s** |
| `cargo test -p saussunet-core --test pipeline_raw` | 1 passed (`ingest_raw_end_to_end`) |
| `cargo test -p saussunet-core --test watch_webdav interval_loop` | 1 passed |
| `cargo test -p saussunet-core --test watch_webdav restart` | 1 passed |
| `cargo build -p saussunet-core` | Finished dev profile, 0 warnings surfaced |
| `bash scripts/check.sh` (full gate incl. clippy, fmt, tests, axum-absence guard, write-funnel guard, release build) | **`check: OK`** |

No flake re-runs needed: the known `queue`/`pipeline_url` SQLITE_BUSY family did not reproduce in any run.

---

## Success Criterion 1 — TRUE
> User configures a WebDAV folder as a watched source; new, changed, and removed files are detected on the poll interval (etag/size, Depth:1 walk with a depth cap) and ingestion jobs are enqueued automatically.

| Facet | Test (actual run line) |
|-------|------------------------|
| Config declares a watched source | `test config::tests::full_watch_section_parses ... ok`, `bare_watch_section_defaults ... ok`, `watch_resolves_to_runtime_list ... ok` |
| New file → job enqueued | `test new_file_enqueues_ingest_job ... ok` |
| Changed file (etag/size) → re-enqueue | `test changed_file_reenqueues ... ok`; pure classifier `watch::diff::tests::changed_on_etag_mismatch_even_weak ... ok` |
| Removed file → log + state prune | `test removed_file_logs_and_prunes_state ... ok` |
| Depth:1 walk with depth cap | `test walk_depth_cap ... ok`; code check: `dav.rs:116` `.header("Depth", "1")` is the only PROPFIND depth (sync REPORT uses `Depth: 0` per RFC 6578, not a walk) |
| Detected on poll interval | `test interval_loop_polls_on_spawn_and_repeats ... ok` (first poll on spawn + repeats) |

## Success Criterion 2 — TRUE
> Watcher state (etags, pageTokens, last-check) persists in SQLite: restarting the server resumes watching with no full re-scan.

| Facet | Test (actual run line) |
|-------|------------------------|
| Headline: restart resumes, zero re-scan (file-backed SQLite) | `test restart_resumes_without_rescan ... ok` |
| Per-file etag/size/sha256 rows persist | `test store::watch_state::tests::watch_row_roundtrip ... ok`, `replace_watch_rows_swaps_atomically ... ok`, `delete_watch_row_removes_only_its_key ... ok` |
| pageTokens (sync token) persist + reuse | `test sync_token_persisted_and_reused ... ok`, `store::sync_state::tests::set_sync_token_and_flag_roundtrip ... ok` |
| last-check / last_error bookkeeping | `test sync_state_updated_after_walk ... ok`, `poll_failure_records_last_error ... ok`, `record_sync_poll_preserves_token_and_sha256 ... ok` |
| Migration 0009 applies cleanly (T16) | `migrations/0009_watch_state.sql` present; applied automatically via `run_migrations` (`store/mod.rs:62`) on every `Store::connect` — all 282 tests that open a store pass |

## Success Criterion 3 — TRUE
> Re-polling a folder of unchanged files enqueues nothing (sha256-gated enqueue).

| Facet | Test (actual run line) |
|-------|------------------------|
| Unchanged tree → zero jobs, log.md byte-identical | `test unchanged_poll_is_noop ... ok` |
| Weak-etag rotation, same bytes → no job (sha256 gate) | `test weak_etag_same_bytes_no_enqueue ... ok` |
| Pure diff: etag+size equal → nothing | `watch::diff::tests::unchanged_when_etag_and_size_equal ... ok` |
| Skip-noted once, not every poll | `test skipped_ext_logged_once_not_every_poll ... ok` |

---

## Requirements traceability

### SRCC-01 (watched WebDAV source, interval poll, etag/size detection, Depth:1 + cap, enqueue)
- **Implemented by:** 04-01 (`[[watch]]` config), 04-03 (parser + diff), 04-04 (DavClient + poll_once + ingest_raw), 04-05 (full walk), 04-06 (interval loop + boot wiring)
- **Traced to tests:** `bare_watch_section_defaults`, `full_watch_section_parses`, `watch_url_rejects_bad_forms`, `duplicate_watch_urls_rejected`, `watch_requires_whole_credentials`, `zero_interval_rejected`, `zero_depth_cap_rejected`, `watch_password_never_in_debug`, `committed_example_config_matches_default_config`, `parses_plain_propfind` + 6 more multistatus tests, `new_file_enqueues_ingest_job`, `changed_file_reenqueues`, `removed_file_logs_and_prunes_state`, `walk_depth_cap`, `interval_loop_polls_on_spawn_and_repeats`, `extension_allowlist_routing`, `ingest_raw_end_to_end` — **all ok**

### SRCC-02 (watcher state persists in SQLite; sha256-gated enqueue; pageTokens)
- **Implemented by:** 04-02 (migration 0009 + watch_state store), 04-05 (sha256 gate), 04-06 (restart proof), 04-07 (RFC 6578 sync tokens + latch)
- **Traced to tests:** `watch_row_roundtrip`, `replace_watch_rows_swaps_atomically`, `delete_watch_row_removes_only_its_key`, `set_sync_token_and_flag_roundtrip`, `record_sync_poll_preserves_token_and_sha256`, `restart_resumes_without_rescan`, `sync_token_persisted_and_reused`, `sync_unsupported_falls_back_forever`, `stale_token_resyncs_not_latches`, `unchanged_poll_is_noop`, `weak_etag_same_bytes_no_enqueue` — **all ok**

## Critical-case inventory coverage (T1–T16): 16/16 green

| Case | Test | Result |
|------|------|--------|
| T1 new-file enqueue | `new_file_enqueues_ingest_job` | ok |
| T2 changed-file re-enqueue | `changed_file_reenqueues` | ok |
| T3 removal log+prune | `removed_file_logs_and_prunes_state` | ok |
| T4 depth cap, never Depth:∞ | `walk_depth_cap` (+ `Depth: "1"` header audit) | ok |
| T5 interval + first poll | `interval_loop_polls_on_spawn_and_repeats` | ok |
| T6 restart no re-scan | `restart_resumes_without_rescan` | ok |
| T7 sync_state persist + last_error | `sync_state_updated_after_walk`, `poll_failure_records_last_error` | ok |
| T8 sync-token round-trip + latch | `sync_token_persisted_and_reused`, `sync_unsupported_falls_back_forever`, `stale_token_resyncs_not_latches` | ok |
| T9 unchanged poll silent + zero jobs | `unchanged_poll_is_noop` | ok |
| T10 weak-etag sha256 gate | `weak_etag_same_bytes_no_enqueue` | ok |
| T11 allowlist + skip-logged-once | `extension_allowlist_routing`, `skipped_ext_logged_once_not_every_poll` | ok |
| T12 password never in Debug/errors | `watch_password_never_in_debug`, `error_string_has_no_password` | ok |
| T13 config defaults/validation + example byte-identity | `bare_watch_section_defaults`, `watch_url_rejects_bad_forms`, `committed_example_config_matches_default_config` | ok |
| T14 multistatus parser edge cases | `watch::multistatus::tests::*` (7 tests incl. weak etag, 404-propstat, traversal href, sync-report) | ok |
| T15 ingest_raw handler (MockProvider) | `ingest_raw_end_to_end` | ok |
| T16 migration 0009 applies cleanly | all `Store::connect` suites green (282/282) | ok |

## must_haves key_links spot-check (all 7 plans)

| Plan | Key link | Verified |
|------|----------|----------|
| 04-01 | `FileConfig.watch → resolve() → RuntimeConfig.watch` | `watch_resolves_to_runtime_list ... ok` |
| 04-02 | `sqlx::migrate!` picks up 0009 on connect; crash-safe order | `store/mod.rs:62 run_migrations`; `watch/mod.rs:37` doc + `upsert_watch_row` after `insert_job` (lines 166/210) |
| 04-03 | `poll_once = parse_multistatus → diff_walk` | wired in `watch/mod.rs`; unit tests green |
| 04-04 | `poll_once → diff → submit_raw → handle_job("ingest_raw")` | `new_file_enqueues_ingest_job` + `ingest_raw_end_to_end` |
| 04-05 | `append_entry` sole log writer; `delete_watch_row` complete-walk only | `watch/mod.rs:245` single `append_entry` per poll; removal pass gated on walk completion |
| 04-06 | `boot() → spawn_watcher` per entry, aborted on ctrl_c | `main.rs:177–210` (watcher handles spawned after worker pool, same abort loop) |
| 04-07 | `poll_once → get_sync_state → sync_collection/fallback → set_sync_token/set_sync_unsupported` | `watch/mod.rs:297–318`; T8 tests green |

---

## Human verification (required — cannot be automated in CI)

| # | Item | Requirement | Instructions (from 04-VALIDATION.md) | Status |
|---|------|-------------|--------------------------------------|--------|
| H1 | Poll against a real LAN WebDAV server (Apache mod_dav / Nextcloud) | SRCC-01 | Point `[[watch]]` at a real server, add/change/remove a file, observe `ingest_raw`/`ingest_pdf` jobs + `log.md` entries; verify real-server header quirks the stub cannot reproduce | ⬜ pending — **human_needed** |

## Gaps

None found in automated scope. All success criteria, all T1–T16 cases, all key_links, and the full gate pass. The only open item is H1 (real-server UAT), which is inherently manual by design of the validation plan.

**Score rationale:** 5/5 — every goal-backward success criterion is backed by an executed, passing test with observed output; the residual human item is a pre-agreed manual-only verification, not a gap in implementation or test coverage.
