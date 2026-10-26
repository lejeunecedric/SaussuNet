---
phase: 02-llm-provider-durable-job-queue
reviewed: 2026-10-26T08:36:06Z
depth: standard
files_reviewed: 19
files_reviewed_list:
  - config.toml
  - crates/saussunet-core/Cargo.toml
  - crates/saussunet-core/src/config.rs
  - crates/saussunet-core/src/error.rs
  - crates/saussunet-core/src/lib.rs
  - crates/saussunet-core/src/llm/mock.rs
  - crates/saussunet-core/src/llm/mod.rs
  - crates/saussunet-core/src/llm/openai_compat.rs
  - crates/saussunet-core/src/queue/mod.rs
  - crates/saussunet-core/src/store/jobs.rs
  - crates/saussunet-core/src/store/mod.rs
  - crates/saussunet-core/tests/init_vault.rs
  - crates/saussunet-core/tests/llm_live.rs
  - crates/saussunet-core/tests/llm_provider.rs
  - crates/saussunet-core/tests/queue.rs
  - crates/saussunet-core/tests/queue_store.rs
  - crates/saussunet/src/main.rs
  - migrations/0005_jobs_next_attempt_at.sql
  - scripts/check.sh
findings:
  critical: 0
  warning: 3
  info: 5
  total: 8
status: issues_found
---

# Phase 02: Code Review Report

**Reviewed:** 2026-10-26T08:36:06Z
**Depth:** standard
**Files Reviewed:** 19
**Status:** issues_found

## Summary

Scope: commits `4f98dd6..b7216e6` (plans 02-01 → 02-05) — `[llm]` config + `SecretString`, `LlmProvider` trait + tolerant JSON extraction + repair round, `MockProvider`, `OpenAiCompat` HTTP provider, durable queue store methods (claim/touch/complete/fail/reap) + migration 0005, bounded worker pool with heartbeat, daemon wiring in `main.rs`, and the zero-real-LLM CI guard.

Overall the work is careful: all SQL is parameterized, the api-key secrecy discipline (D-03) is consistently enforced (no body interpolation in errors, manual `Debug` redacts, `SecretString` never prints), SSRF is bounded (URL built only from operator config at construction), the repair echo is length-bounded, and the test suite is deterministic with injected clocks.

The key concern is that **claim transitions have no fencing token**: `complete_job` / `fail_job` / `touch_claim` match on `id` (and `status='claimed'`) but never verify *which* claim they belong to. A stale worker whose lease was reaped can clobber a live worker's claim. Secondary concerns: no validation of `[llm]` numeric config (`max_concurrency = 0` silently spawns zero workers), and the default `provider = "mock"` daemon dead-letters every job it claims after 4 invisible failures.

Security scan result: no hardcoded secrets (only placeholder values in gitignored-template comments), no injection surfaces, no shell/eval, no path derived from untrusted payload. No source files were modified by this review.

## Warnings

### WR-01: No claim fencing — a stale worker can clobber a live claim in `fail_job` / `complete_job`

**File:** `crates/saussunet-core/src/store/jobs.rs:146-186`, `130-141`; `crates/saussunet-core/src/queue/mod.rs:183-185`
**Issue:** `fail_job` updates `WHERE id = ?` with no `status='claimed'` guard, and reads `attempts` in a separate non-transactional `SELECT` before the `UPDATE`. `complete_job` guards on `status='claimed'` but cannot tell *whose* claim it is. Scenario: worker A's heartbeat `touch_claim` fails transiently (errors are only logged, queue/mod.rs:183-185) or A stalls past `claim_expiry`; the reaper returns the job to `pending`; worker B claims it; A's provider call then fails → A's `fail_job` increments `attempts`, sets `status='pending'`, `claimed_at=NULL` — destroying B's live claim. B's later `complete_job` affects 0 rows (only a warn log), and the job is retried despite having succeeded → duplicate provider side effects and corrupted retry accounting. The same fencing gap lets A's `complete_job` mark *B's* in-flight claim `done`. The abort-based crash test (`queue.rs:182`) covers the clean-kill case but not this stale-worker race.
**Fix:** Fence every claim transition on the claim stamp returned by `claim_jobs`' `RETURNING` (or add a `claim_token` column):
```sql
-- claim_jobs already RETURNING claimed_at; carry it as `claim_ts: String`:
UPDATE jobs SET status='done', finished_at=?
 WHERE id=? AND status='claimed' AND claimed_at=?;   -- claim_ts
-- fail_job: wrap SELECT+UPDATE in one tx and add the same
--   AND status='claimed' AND claimed_at=? guard.
```
Treat 0-rows-affected in `fail_job` the same way `complete_job` already warns.

### WR-02: `[llm]` numeric config is unvalidated — `max_concurrency = 0` silently spawns zero workers

**File:** `crates/saussunet-core/src/config.rs:87-90`, `crates/saussunet-core/src/queue/mod.rs:79-90`, `crates/saussunet/src/main.rs:135-144`
**Issue:** `max_concurrency = 0` in config makes `(0..0)` spawn no workers; the daemon prints `queue ready: workers=0` and jobs accumulate forever with zero errors. `timeout_secs = 0` is likewise accepted and gives reqwest a zero-duration client timeout (every request fails). Nothing in `resolve()` or `boot()` rejects these.
**Fix:** Clamp or reject at resolve/boot time:
```rust
if self.llm.max_concurrency == 0 { return Err(CoreError::Config("llm.max_concurrency must be >= 1".into())); }
if self.llm.timeout_secs == 0 { return Err(CoreError::Config("llm.timeout_secs must be > 0".into())); }
```

### WR-03: Default `provider = "mock"` daemon dead-letters every claimed job

**File:** `crates/saussunet-core/src/llm/mod.rs:46`, `crates/saussunet-core/src/store/jobs.rs:158-168`
**Issue:** `provider_from_config("mock")` builds an empty-script `MockProvider`, so every `generate_json` in the running daemon fails with "mock script exhausted" → `fail_job` backoff → after 4 invisible failures the job is permanently `status='dead'`. With `provider = "mock"` being the shipped default (config.toml), any job the Phase-3+ pipeline enqueues on a default-config daemon is silently dead-lettered, and `dead` requires manual resurrection. The comment says mock mode "only proves plumbing", but the failure mode is indistinguishable from a real outage in the dead-letter table.
**Fix:** Either refuse to spawn workers in mock daemon mode (log "mock provider: queue draining disabled"), or have the worker skip claiming when the provider is the empty-script mock, so jobs stay `pending` until a real provider is configured.

## Info

### IN-01: `OpenAiCompat` retries *every* HTTP 400 without `response_format`

**File:** `crates/saussunet-core/src/llm/openai_compat.rs:111-119`
**Issue:** Because response bodies are (correctly) never read for secrecy, a genuine 400 (invalid model, malformed request) is indistinguishable from a `response_format` rejection, so every 400 doubles the outbound request and the surfaced error reports only the second attempt's status.
**Fix:** Acceptable trade-off as documented; consider logging the first-attempt status distinctly (e.g. `tracing::debug!("400 with response_format, retrying without")`) so operators can tell the two cases apart.

### IN-02: `schema` is never enforced — repair triggers only on unparseable JSON

**File:** `crates/saussunet-core/src/llm/mod.rs:80-100`
**Issue:** `generate_json` embeds `schema` in the repair prompt text but never validates the returned value against it; valid JSON that violates the schema never triggers the D-06 repair round and flows through unchecked (also via `generate_json_typed` when the Rust type is looser than the schema). Spec-consistent ("unparseable → repair") but worth noting before Phase 3 relies on schema conformance.
**Fix:** When a JSON-Schema validator (or hand-rolled check) is available, validate after parse and route mismatch into the same single-repair path.

### IN-03: `extract_json_block` accepts non-object JSON

**File:** `crates/saussunet-core/src/llm/mod.rs:122-126`
**Issue:** The whole-string fast path returns `Some(trimmed)` for any valid JSON, including scalars/arrays (`"42"`, `"[1,2]"`), even though the contract says "pull the balanced `{...}` block". A scalar then parses fine and is returned as the "JSON object".
**Fix:** Gate the fast path on `trimmed.starts_with('{')` (or check the parsed value is an object).

### IN-04: zero-real-LLM CI guard is string-grep fragile

**File:** `scripts/check.sh` (guard block, lines added in 02-05)
**Issue:** The guard greps test files for the literal `OpenAiCompat`. A comment mentioning the word trips a false positive, while a real call reached indirectly (e.g. `provider_from_config` with an `openai-compatible` config) passes the grep. The `env -u SAUSSUNET_LLM__API_KEY cargo test` half is sound.
**Fix:** Keep the grep but additionally assert no test constructs a config with `provider = "openai-compatible"`, or move the guard into a test that scans test sources at runtime.

### IN-05: `fail_job` dead branch leaves `claimed_at` set; ctrl-C aborts mid-call

**File:** `crates/saussunet-core/src/store/jobs.rs:159-168`; `crates/saussunet/src/main.rs:147-150`
**Issue:** Cosmetic/operational: (a) the dead-letter `UPDATE` sets `finished_at` but not `claimed_at=NULL`, so dead rows carry a stale claim stamp; (b) shutdown `h.abort()`s workers mid-provider-call — intentional kill-9 semantics (claims expire after 5 min), but a short graceful drain (`select!` on ctrl_c vs. in-flight completion, bounded) would avoid re-paying for an almost-finished LLM call.
**Fix:** Add `claimed_at=NULL` to the dead-letter UPDATE; optionally drain in-flight jobs with a bounded grace period before aborting.

---

_Reviewed: 2026-10-26T08:36:06Z_
_Reviewer: Claude (gsd-code-reviewer)_
_Depth: standard_
