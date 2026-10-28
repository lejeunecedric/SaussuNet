---
phase: 02-llm-provider-durable-job-queue
fixes: 02-REVIEW.md
status: all_fixed
findings_in_scope: 3
fixed: 3
skipped: 0
iteration: 1
date: 2026-10-28
---

# Phase 02: Review Fix Report

Scope: the 3 Warning findings from `02-REVIEW.md` (0 Critical). Info findings
(IN-01..IN-05) left untouched per fix scope. Baseline before fixes:
121 passed / 0 failed / 1 ignored. Final: **127 passed / 0 failed / 1 ignored**
(the ignored live test was never run), `scripts/check.sh` → `check: OK`.

## WR-01: No claim fencing — stale worker could clobber a live claim

**Status: fixed** — commit `d2c21b1` (`fix(02): WR-01 fence job state transitions on claim identity`)

**What was changed:** Added a dedicated `claim_token` fencing column rather than
reusing the `claimed_at` stamp: heartbeats overwrite `claimed_at` every 60 s
(D-11), so a claim-time stamp could not serve as a stable token for a live
worker (same reasoning migration 0005 used to reject `claimed_at` reuse for
backoff math).

- `migrations/0006_jobs_claim_token.sql`: `ALTER TABLE jobs ADD COLUMN claim_token TEXT`.
- `claim_jobs` mints a fresh token per claim (`lower(hex(randomblob(16)))`,
  no new deps) and returns it via `RETURNING`; `Job` carries `claim_token`.
- `complete_job` / `touch_claim` / `fail_job` signatures take `claim_token: &str`
  and every UPDATE carries `AND status='claimed' AND claim_token=?`.
- `fail_job`'s SELECT + UPDATE now run in one transaction (the review's
  non-transactional read is gone); a token mismatch returns the new
  `JobOutcome::ClaimLost` and touches nothing — the worker warns, mirroring
  `complete_job`'s existing 0-row warning.
- `reap_expired_claims` and the retry/dead transitions clear `claim_token`.
- `queue/mod.rs`: `process_job` carries the token from the claimed `Job`
  through the provider call, heartbeat (`touch_claim` fenced too — a stale
  claimant can no longer extend someone else's lease), complete, and fail.

**Files:** `migrations/0006_jobs_claim_token.sql`, `src/store/jobs.rs`,
`src/queue/mod.rs`, `tests/queue_store.rs`, `tests/queue.rs`.

**Test evidence:** TDD — regression test `stale_claimant_cannot_clobber_live_claim`
written first and observed RED on the unfenced code (`complete_job` affected
1 row, clobbering worker B's live claim), GREEN after the fix: A's stale
`complete_job` → 0 rows, A's stale `fail_job` → `ClaimLost` with attempts and
B's `claimed_at` untouched, B's own complete lands. Existing store/queue tests
updated to the token API preserving intent (each retry now claims → fails on
its own claim, matching real worker flow). `queue_store` 8 passed, `queue` 6
passed.

## WR-02: `[llm]` numeric config unvalidated

**Status: fixed** — commit `871bc85` (`fix(02): WR-02 validate [llm] numeric config at resolve time`)

**What was changed:** Validation (not clamping) at resolve time, matching the
review's preferred option and the repo's `CoreError::Config` style.
`FileConfig::resolve()` now returns `Result<RuntimeConfig>` and rejects:
- `max_concurrency = 0` → `"llm.max_concurrency must be >= 1 (0 would spawn zero workers)"`
- `timeout_secs = 0` → `"llm.timeout_secs must be > 0 (0 would fail every provider request)"`

Call sites updated: `main.rs` boot/command paths (`resolve()?` — a bad config
now fails boot loudly instead of printing `workers=0`), and the seven test
fixtures (`resolve().expect("resolve")`).

**Files:** `src/config.rs`, `crates/saussunet/src/main.rs`, test fixtures in
`tests/{queue,queue_store,writer,reindex,gen_hash,phase1_acceptance,init_vault}.rs`.

**Test evidence:** 3 new unit tests: `zero_max_concurrency_is_rejected`,
`zero_timeout_secs_is_rejected` (both assert the error names the offending
key), `positive_llm_numerics_resolve` (minimum valid values pass). All config
tests green.

## WR-03: Default `provider = "mock"` dead-lettered every claimed job

**Status: fixed** — commit `bd9f7cc` (`fix(02): WR-03 mock daemon no longer dead-letters the queue`)

**What was changed:** Chose the review's first suggested option — refuse to
spawn workers in mock daemon mode (the simplest honest fix; jobs stay
`pending` instead of silently dead-lettering). New core helper
`llm::daemon_drains_queue(rt) -> bool` returns `false` for
`provider = "mock"`; `boot()` skips `spawn_workers` in that case and prints
`mock provider: queue draining disabled — jobs stay pending until a real
[llm].provider is configured`. The gate lives at the daemon boot boundary
only — tests keep driving workers with scripted `MockProvider`s via
`spawn_workers` unchanged (D-13/D-14 unaffected). The `default_config()`
template and committed `config.toml` document the behavior.

**Files:** `src/llm/mod.rs`, `crates/saussunet/src/main.rs`, `src/lib.rs`,
`config.toml`.

**Test evidence:** 2 new unit tests: `mock_daemon_does_not_drain_queue`,
`real_provider_daemon_drains_queue`. All existing worker-pool tests
(`tests/queue.rs`, 6) still pass — scripted mocks drain normally through
`spawn_workers`.

## Verification

- `cargo test -p saussunet-core`: **127 passed / 0 failed / 1 ignored**
  (baseline 121 + 6 new tests: 1 WR-01 regression, 3 WR-02, 2 WR-03).
- `bash scripts/check.sh`: fmt, clippy `-D warnings`, zero-real-LLM guard,
  tests, axum guard, release build → **check: OK**.
- Commits: `d2c21b1` (WR-01), `871bc85` (WR-02), `bd9f7cc` (WR-03); working
  tree clean apart from this uncommitted report. Not pushed.
