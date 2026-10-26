---
phase: 02-llm-provider-durable-job-queue
plan: 04
subsystem: job-queue, worker-pool
tags: [rust, tokio, tokio-select, heartbeat, claim-expiry, kill-9-simulation, bounded-pool, mock-provider]

requires:
  - phase: 02-llm-provider-durable-job-queue (plan 02)
    provides: "claim_jobs/touch_claim/complete_job/fail_job/reap_expired_claims store ops, JobOutcome, injected-clock convention"
  - phase: 02-llm-provider-durable-job-queue (plan 03)
    provides: "LlmProvider trait (dyn-compatible), MockProvider Scripted Hang/SlowOk + max_concurrent_calls gauge"
  - phase: 01-workspace-foundations-vault-writer
    provides: "jobs table + idx_jobs_claim, file-backed TempDir test fixture, boot_open"
provides:
  - "WorkerConfig (max_concurrency=1 D-04, poll 1s, heartbeat 60s, claim expiry 5min D-11 — injectable ms scales)"
  - "spawn_workers: exactly N tokio claim loops, claim-1-per-iteration, poll fallback, join handles for shutdown"
  - "worker_loop: reap → claim(1) → process; heartbeat select! branch; Err → fail_job, Ok → complete_job; store errors logged, loop survives"
  - "tests/queue.rs: 6 integration tests proving SC3 crash-resume, D-04 bound, D-11 heartbeat, D-09/D-10 through the worker, D-12 no-duplication"
affects: [02-05, daemon-wiring, extraction-pipeline, provider-factory]

tech-stack:
  added: []
  patterns:
    - "Abort-safe heartbeat: tokio::select! { provider_call, heartbeat_loop } inside the worker task — dropping/aborting the task provably stops the lease refresh (a detached tokio::spawn child would survive the parent's abort)"
    - "Kill-9 simulation = JoinHandle::abort: heartbeat stops, in-flight call abandoned, claim goes stale, reaper returns the job"
    - "Bounded-pool proof via running-max gauge: 2×N jobs with SlowOk(delay) responses, assert max_concurrent_calls() == N (reaches bound AND never exceeds)"

key-files:
  created:
    - crates/saussunet-core/src/queue/mod.rs
    - crates/saussunet-core/tests/queue.rs
  modified:
    - crates/saussunet-core/src/lib.rs

key-decisions:
  - "PLANNER (recorded): pool shape = N independent tokio::spawn claim loops, claim-1-per-iteration, poll-interval fallback — durability lives in the DB row, polling survives restart with zero extra state (PATTERNS A4); chosen over semaphore/mpsc"
  - "Heartbeat runs as a select! branch inside the worker task, NOT a spawned+aborted child task: the plan's own kill-9 must-have (abort == heartbeat stops) cannot hold for a detached child, which survives JoinHandle::abort on the parent"
  - "Heartbeat test clears the real D-09 30s gate via SQL after the Hang failure to observe the done path in-budget — the gate itself is already proven at store level by 02-02 backoff_gates_claim"
  - "Control (un-heartbeated) claim parked in a second vault (ensure_vault(2)) so the live worker polling vault 1 can never race with the reap-control assertion"

requirements-completed: [LLMP-02, SRCC-03]

# Metrics
duration: 15min
completed: 2026-10-26
---

# Phase 2 Plan 04: Pipeline Slice — Bounded Worker Pool + Live Heartbeat Summary

**`queue/` module wires plan 02-02's store ops to plan 02-03's provider through one thin worker — N bounded claim loops with an abort-safe select! heartbeat (D-11), reap→claim→generate_json→complete/fail per iteration — and 6 integration tests prove SC3 crash-resume (kill-9 via JoinHandle::abort → reap → re-claim → done exactly once) and the D-04 bound (gauge == N with 2×N SlowOk jobs) in ~1.1s of wall clock.**

## Performance

- **Duration:** 15 min
- **Started:** 2026-10-26T08:07:22Z
- **Completed:** 2026-10-26T08:22:13Z
- **Tasks:** 2
- **Files modified:** 3 (2 created, 1 modified)

## Accomplishments
- `spawn_workers(store, provider, vault_id, cfg)` spawns exactly `cfg.max_concurrency` claim loops (single `(0..N).map(tokio::spawn)` — D-04 bound is structural); `WorkerConfig::default()` carries the D-11 values verbatim (heartbeat 60s, expiry 300s) with all durations injectable so tests run on 10–50ms scales.
- `worker_loop` per iteration: `reap_expired_claims` → `claim_jobs(1)` → process; provider Err routes through `fail_job` (D-09/D-10 live in the store), Ok through `complete_job`; every store error is `tracing::error!`-logged and the loop continues — one bad iteration never kills a worker.
- SC3 headline proof (`crash_mid_job_resumes_without_duplication`): worker A hangs mid-call, `JoinHandle::abort` (exact kill-9 equivalent — heartbeat branch dropped, lease goes stale), worker B's reaper returns the job after expiry, B completes it — status done, `finished_at` stamped once, `call_count == 2` (hang + success, no third execution), attempts 0 (reap preserves, abort never calls fail_job).
- D-04 bound proof (`pool_is_bounded`): N=2 workers, 4 jobs of `SlowOk(50ms)` — `max_concurrent_calls() == 2` proves the pool reaches real parallelism AND (running-max gauge) never exceeds the bound.
- D-11 heartbeat proof (`heartbeat_keeps_live_job_unreaped`): test-driven `reap_expired_claims` reaps 0 rows while a hung job's 50ms heartbeats keep `claimed_at` fresh past the 300ms expiry; the same window reaps a control claim that never heartbeated (parked in vault 2 to avoid racing the live worker).
- D-09/D-10 through the worker without real waits: worker-visible Fail{500} lands `next_attempt_at` within now+28..32s (asserted, no 30s sleep); three injected-clock `fail_job` calls then dead-letter with the error retained.
- Prompt built as `format!("job kind: {}\ndata:\n{}", kind, payload_json)` — untrusted payload stays user-message data, never system instructions (threat model); logs carry job id/kind/error strings only (D-03).

## Task Commits

Each task was committed atomically:

1. **Task 1: Worker pool + heartbeat (queue module)** — `19104ab` (feat)
2. **Task 2: Crash-resume, bounded-pool, heartbeat, and retry integration tests** — `4387e2b` (test)

**Plan metadata:** this SUMMARY commit (docs)

## Files Created/Modified
- `crates/saussunet-core/src/queue/mod.rs` — `WorkerConfig` (+ manual Default with D-04/D-11 values), `spawn_workers`, `worker_loop`, `process_job`, `heartbeat` (select! branch), 1 unit test on defaults
- `crates/saussunet-core/tests/queue.rs` — 6 tests: `job_flows_through_worker_to_done`, `pool_is_bounded`, `crash_mid_job_resumes_without_duplication`, `heartbeat_keeps_live_job_unreaped`, `provider_failure_routes_through_backoff_then_dead`, `concurrent_workers_no_duplication`
- `crates/saussunet-core/src/lib.rs` — `pub mod queue;` (main.rs untouched — daemon wiring is plan 02-05)

## Decisions Made
- Pool shape recorded as the planner decision verbatim: N independent spawn loops + poll fallback (durability in the DB row, restart-safe with zero extra state).
- Heartbeat as an in-task `tokio::select!` branch instead of the plan's "spawn heartbeat task … abort heartbeat": a detached `tokio::spawn` child SURVIVES `JoinHandle::abort()` on the worker task, so the plan's own kill-9 must-have ("task abort == heartbeat stops") is only implementable this way. Normal-path behavior is identical (heartbeat stops the moment the provider call returns).
- The heartbeat test's "after Hang resolves the job completes done" step clears `next_attempt_at` via SQL: the Hang resolves as a 500 → real D-09 30s gate, which cannot elapse inside the test budget; the gate itself is already proven at store level (02-02 `backoff_gates_claim`).
- Control assertion for the reap test parked in a second vault (`ensure_vault(2, "control")`) so the live worker polling vault 1 can never claim the stale control row.

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Plan mismatch] Heartbeat implemented as select! branch, not spawned+aborted task**
- **Found during:** Task 1 design (before implementation)
- **Issue:** Plan said "spawn heartbeat task … abort heartbeat", but tokio task abort does not cancel separately-spawned children — the kill-9 simulation (Task 2 test 3) requires the heartbeat to die with the worker task.
- **Fix:** Heartbeat loop races the provider call inside `tokio::select!` in `process_job`; aborting the worker drops both futures. Rationale documented in the module header and `process_job` comment.
- **Files modified:** crates/saussunet-core/src/queue/mod.rs
- **Verification:** `crash_mid_job_resumes_without_duplication` green — after abort the claim goes stale and is reaped (only possible if the heartbeat stopped).
- **Committed in:** 19104ab (Task 1 commit)

**2. [Rule 1 - API mismatch] `reap_expired_claims` takes `chrono::TimeDelta`, config holds `std::time::Duration`**
- **Found during:** Task 1 (`cargo build` E0308)
- **Issue:** Plan's worker sketch passed `cfg.claim_expiry` (std Duration) directly; the 02-02 store API takes chrono TimeDelta.
- **Fix:** `chrono::TimeDelta::from_std(cfg.claim_expiry)` conversion at the call site (fallback to 300s on out-of-range, unreachable for sane configs).
- **Files modified:** crates/saussunet-core/src/queue/mod.rs
- **Verification:** build + clippy clean; expiry behavior proven by tests 3 and 4.
- **Committed in:** 19104ab (Task 1 commit)

**3. [Rule 1 - Plan mismatch] Heartbeat test cannot observe 'done' without clearing the D-09 gate**
- **Found during:** Task 2 (test 4 design)
- **Issue:** Plan's "after Hang resolves the job completes done" conflicts with D-09: Hang resolves as Err(500) → fail_job → real 30s `next_attempt_at` gate, unobservable inside the <30s test budget.
- **Fix:** Test waits for the worker's fail_job to land (pending), clears `next_attempt_at` via SQL (gate already proven at store level in 02-02), then asserts done + call_count==2. Comment in test records the rationale.
- **Files modified:** crates/saussunet-core/tests/queue.rs
- **Verification:** test green in ~0.6s.
- **Committed in:** 4387e2b (Task 2 commit)

**4. [Rule 1 - Plan clarification] Crash-path attempts semantics asserted as 0**
- **Found during:** Task 2 (test 3)
- **Issue:** Plan said "attempts reflects the crash path per fail_job/reap semantics" without a number; actual committed semantics (02-02): reap preserves attempts and an aborted worker never calls fail_job.
- **Fix:** Assert `attempts == 0` with an explanatory comment (the crash path adds no retry charge).
- **Files modified:** crates/saussunet-core/tests/queue.rs
- **Verification:** test green, deterministic across repeated runs.
- **Committed in:** 4387e2b (Task 2 commit)

**5. [Process] `scripts/check.sh` run once at plan end** (same consolidation as plans 02-01..02-03 — it embeds a full release build; per-task verification used `cargo build`/`cargo test`/`cargo clippy --all-targets -D warnings` + acceptance greps).

---

**Total deviations:** 4 auto-fixed (2 plan-vs-tokio/store-API adaptations, 2 test-budget adaptations) + 1 process consolidation
**Impact on plan:** No queue or provider semantics changed; every must-have truth and acceptance criterion verified as written. The select! heartbeat is strictly stronger than the sketched spawn+abort (abort-safety is provable). No scope creep.

## Issues Encountered
- None beyond the deviations above. Pool-bound and crash-resume tests were run 3× consecutively to check flake-stability (all ~0.9s, stable).

## Verification Evidence
- `cargo test -p saussunet-core --test queue`: **6 passed, 0 failed** in ~1.1s (< 30s budget), stable across 3 runs
- `env -u SAUSSUNET_LLM__API_KEY cargo test -p saussunet-core`: **119 passed, 0 failed** (was 112 at plan start: +6 queue tests +1 WorkerConfig defaults unit test) — D-14 zero-key proof
- `cargo clippy -p saussunet-core --all-targets -- -D warnings`: clean; `cargo fmt --check`: clean; `bash scripts/check.sh`: **check: OK** (fmt + clippy + tests + axum-absence guard + release build)
- Acceptance greps: `pub struct WorkerConfig` / `pub fn spawn_workers` / `Duration::from_secs(60)` / `Duration::from_secs(300)` in queue/mod.rs; single `(0..cfg.max_concurrency)` spawn loop; worker calls `reap_expired_claims`/`claim_jobs`/`touch_claim`/`complete_job`/`fail_job`; `pub mod queue;` in lib.rs; all 6 named test fns present in tests/queue.rs; crash test uses `abort()` + `call_count(), 2`; pool test asserts `max_concurrent_calls(), 2` with 4× SlowOk(50ms); backoff test asserts now+28..32s window and `JobOutcome::Dead` with error containing "500"

## Next Phase Readiness
- Plan 02-05 (daemon wiring): `spawn_workers` returns join handles ready for the binary's shutdown path; `provider_from_config` factory + `[llm].max_concurrency` feed `WorkerConfig` directly.
- Phase 3 (extraction): job kinds/payloads plug into the existing worker prompt; `generate_json_typed` is the typed seam when workers need structured results instead of `serde_json::Value`.
- Note for 02-05: `WorkerConfig` should be built from the resolved `[llm]` section (`max_concurrency`) with the D-11 duration defaults left untouched.

## Self-Check: PASSED

---
*Phase: 02-llm-provider-durable-job-queue*
*Completed: 2026-10-26*
