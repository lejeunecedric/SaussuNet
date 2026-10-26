---
phase: 02-llm-provider-durable-job-queue
plan: 02
subsystem: database, job-queue
tags: [rust, sqlx, sqlite, chrono, tokio, injected-clock, atomic-claim, backoff, dead-letter]

requires:
  - phase: 01-workspace-foundations-vault-writer
    provides: "jobs table schema + idx_jobs_claim (0003), Store pool/WAL/busy_timeout, boot_open test fixture, insert_job/list_jobs"
  - phase: 02-llm-provider-durable-job-queue (plan 01)
    provides: "CoreError::Queue variant, tokio time feature, workspace dep hygiene"
provides:
  - "claim_jobs atomic single-transaction claim with due-gate (D-12 + D-09 gating)"
  - "fail_job D-09 backoff 30s/2min/10min → JobOutcome::Retrying{next_attempt_at} | Dead (D-10 dead-letter retains error)"
  - "touch_claim heartbeat + reap_expired_claims expiry (D-11), attempts preserved"
  - "complete_job claimed→done with finished_at"
  - "migration 0005 next_attempt_at column (documented Phase-1 D-02 exception)"
  - "tests/queue_store.rs: 7 deterministic injected-clock tests, zero real sleeps"
affects: [02-04, worker-pool, extraction-pipeline, phase-5-ui]

tech-stack:
  added: []
  patterns:
    - "Injected-clock store methods: every time-comparing query takes now: DateTime<Utc> formatted '%Y-%m-%d %H:%M:%S' (SQLite datetime('now') wire format)"
    - "UPDATE ... WHERE id IN (SELECT ... ORDER BY priority DESC, created_at, id LIMIT ?) RETURNING inside pool().begin()/commit() for atomic claim"
    - "Outcome enums (JobOutcome) returned from state-transition writes so callers never re-query"

key-files:
  created:
    - migrations/0005_jobs_next_attempt_at.sql
    - crates/saussunet-core/tests/queue_store.rs
  modified:
    - crates/saussunet-core/src/store/jobs.rs
    - crates/saussunet-core/src/store/mod.rs

key-decisions:
  - "PLANNER: retry due-time gating uses NEW migration 0005_jobs_next_attempt_at.sql (numbered 0005 because 0004_pending_proposals.sql exists) — allowed because Phase 1 D-02 froze 'design the schema once', not 'no migrations ever'; reusing claimed_at for backoff is rejected because D-11 heartbeats overwrite claimed_at every 60s, corrupting claimed_at-based due math and making reap-vs-backoff indistinguishable"
  - "PLANNER: all time comparisons take an injected now: DateTime<Utc> parameter formatted '%Y-%m-%d %H:%M:%S' — tokio::time::pause() does NOT advance SQLite's clock, so injected-clock params are the only deterministic test path; production callers pass Utc::now()"
  - "touch_claim/complete_job return rows-affected (u64) so callers/tests can detect no-op transitions (plan left return type open)"
  - "JobOutcome + MAX_RETRIES/BACKOFF_SECS re-exported from store/mod.rs (jobs module is private)"

requirements-completed: [SRCC-03]

duration: 7min
completed: 2026-10-26
---

# Phase 2 Plan 02: Durable Queue-Lifecycle Store Summary

**Five store methods (claim_jobs / touch_claim / complete_job / fail_job / reap_expired_claims) give a jobs row a full durable SQLite life — atomic single-transaction claim with priority order + backoff due-gate, D-09 30s/2min/10min retry ladder via new next_attempt_at column, dead-letter with retained error, and heartbeat/reap lease semantics — proven by 7 deterministic injected-clock tests with zero real sleeps.**

## Performance

- **Duration:** 7 min
- **Started:** 2026-10-26T07:36:48Z
- **Completed:** 2026-10-26T07:43:28Z
- **Tasks:** 2
- **Files modified:** 4 (2 created, 2 modified)

## Accomplishments
- `claim_jobs(vault_id, n, now)` runs the D-12 statement verbatim inside `pool().begin()`/`commit()` with `UPDATE ... WHERE id IN (SELECT ... ORDER BY priority DESC, created_at, id LIMIT ?) RETURNING ...` plus the due gate `(next_attempt_at IS NULL OR next_attempt_at <= ?)`; 8 concurrent tokio claimers over 5 jobs claim each row exactly once (multi-thread runtime, file-backed TempDir DB).
- `fail_job` implements D-09 exactly: attempts++ → `pending` with `next_attempt_at = now + 30s / +120s / +600s` for retries 1/2/3; the 4th failure sets `status='dead'` retaining `error` + `finished_at` (D-10). Returns `JobOutcome::Retrying { next_attempt_at } | Dead` so the worker logs without re-querying.
- `reap_expired_claims` returns claimed rows older than the expiry window to `pending` with `claimed_at` NULL and `attempts` preserved (crashed worker's retries still count toward D-09); `touch_claim` refreshes the lease — heartbeat test proves a touched job survives a reap that an untouched one fails.
- Migration 0005 adds `next_attempt_at TEXT` as a documented exception to Phase 1 D-02 (rationale recorded in the migration header); boot tests green prove it applies cleanly on fresh DBs.
- All SQL uses bind parameters only — no `format!` in any SQL string (threat model: SQL injection HIGH).

## Task Commits

1. **Task 1: Migration 0005 + queue store methods** — `9e736c2` (feat)
2. **Task 2: Deterministic queue-store integration tests** — `dcbfe86` (test)

**Plan metadata:** this SUMMARY commit (docs)

## Files Created/Modified
- `migrations/0005_jobs_next_attempt_at.sql` — `ALTER TABLE jobs ADD COLUMN next_attempt_at TEXT;` with the D-02-exception + claimed_at/heartbeat-conflict rationale
- `crates/saussunet-core/src/store/jobs.rs` — `MAX_RETRIES: i64 = 3`, `BACKOFF_SECS: [i64; 3] = [30, 120, 600]`, `JobOutcome` enum, `ts()` SQLite-format helper, five new `&self` async Store methods
- `crates/saussunet-core/src/store/mod.rs` — re-export `JobOutcome`, `MAX_RETRIES`, `BACKOFF_SECS` (jobs module is private)
- `crates/saussunet-core/tests/queue_store.rs` — 7 tests: `claim_is_atomic_under_concurrent_workers`, `claim_respects_priority_then_created_at`, `retry_backoff_schedule_matches_D09`, `backoff_gates_claim`, `reap_returns_expired_claims_only`, `touch_claim_extends_lease`, `complete_job_transitions`

## Decisions Made
- The two planner decisions (migration-0005 exception; injected-clock parameters) were implemented verbatim and are recorded in frontmatter key-decisions.
- `touch_claim`/`complete_job` return `Result<u64>` (rows affected): the plan left the return type open, and Task 2's "complete on a pending row affects 0 rows" assertion needs it; the worker (02-04) also gets a free lost-claim signal.
- `fail_job` does not filter on `status='claimed'` (plan's UPDATE has no status predicate) — a reaped job's late failure still counts its attempt, which matches D-09's crash-safety intent.

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Planner arithmetic] Concurrency test "remaining 3 pending" assertion is impossible as written**
- **Found during:** Task 2 (`claim_is_atomic_under_concurrent_workers` failed: 0 pending, not 3)
- **Issue:** Plan says insert 5 jobs, spawn 8 claimers × `n=1`, then assert "remaining 3 rows status='pending'" — 8 claimers over 5 jobs claim all 5; 0 remain pending.
- **Fix:** Kept the plan's counts (5 jobs / 8 claimers — stronger contention proof, and the 3 losing claimers correctly get empty results) and asserted 0 pending with an explanatory comment. The D-12 invariant (no id claimed twice, union == 5 distinct) is asserted exactly as planned.
- **Files modified:** crates/saussunet-core/tests/queue_store.rs
- **Verification:** test green, repeated runs deterministic.
- **Committed in:** dcbfe86 (Task 2 commit)

**2. [Rule 1 - Compile/lint] `JobOutcome` unreachable + clippy `non_snake_case` on plan-pinned test name**
- **Found during:** Task 2 (clippy -D warnings)
- **Issue:** `mod jobs` is private, so `JobOutcome`/constants needed a `pub use` in `store/mod.rs` (not in plan's files_modified); `retry_backoff_schedule_matches_D09` violates snake_case but its name is mandated by acceptance criteria.
- **Fix:** Re-exported from `store/mod.rs`; added a targeted `#[allow(non_snake_case)]` with rationale comment.
- **Files modified:** crates/saussunet-core/src/store/mod.rs, tests/queue_store.rs
- **Verification:** `cargo clippy -p saussunet-core --all-targets -- -D warnings` clean.
- **Committed in:** 9e736c2 / dcbfe86

**3. [Process] rustfmt reformat of jobs.rs folded into Task 1 via amend**
- Task 1's committed formatting wasn't `cargo fmt --check` clean; the fmt-only diff was amended into the Task 1 commit (hash 382601b → 9e736c2, pre-push so safe).

---

**Total deviations:** 3 (1 planner-arithmetic fix in a test assertion, 2 mechanical compile/lint necessities)
**Impact on plan:** None of the queue semantics changed; all acceptance criteria and plan-level verification pass. No scope creep.

## Issues Encountered
None beyond the deviations above.

## Verification Evidence
- `cargo test -p saussunet-core --test queue_store`: **7 passed, 0 failed** in 0.07s (zero wall-clock sleeps — `grep -c 'sleep('` → 0)
- `cargo test -p saussunet-core`: **95 passed, 0 failed** total (57 lib + 8 + 6 + 1 + 7 queue_store + 7 + 9) — migration 0005 does not break Phase 1 boot/init tests
- `cargo clippy -p saussunet-core --all-targets -- -D warnings`: clean; `cargo fmt --check`: clean
- `bash scripts/check.sh`: fmt + clippy + tests + axum-absence guard + release build — **check: OK**
- Acceptance greps: migration contains `ALTER TABLE jobs ADD COLUMN next_attempt_at TEXT`; jobs.rs contains `MAX_RETRIES: i64 = 3` and `BACKOFF_SECS: [i64; 3] = [30, 120, 600]`; claim SQL contains `status='claimed'`, `next_attempt_at IS NULL OR next_attempt_at <= ?`, `ORDER BY priority DESC, created_at, id LIMIT ?` inside `begin()`/`commit()`; zero `format!` in jobs.rs

## Next Phase Readiness
- Plan 02-04 (worker pool) can wire claim → heartbeat (`touch_claim` every 60s) → provider call → `complete_job`/`fail_job` directly; `JobOutcome` gives it logging without re-queries, and `claim_jobs(n)` feeds the D-04 pool bound.
- D-09/D-10/D-11(expiry+heartbeat)/D-12 each have a named deterministic test — the worker plan needs no queue-semantics re-proofs.
- Note for 02-04: production callers must pass `Utc::now()` to all five methods (injected-clock convention).

---
*Phase: 02-llm-provider-durable-job-queue*
*Completed: 2026-10-26*
