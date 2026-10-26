//! Bounded worker pool over the durable `jobs` table (LLMP-02, SRCC-03).
//!
//! PLANNER DECISION (02-04): pool shape = N independent `tokio::spawn` claim
//! loops (one task per `max_concurrency` slot), claim-1-per-iteration, with a
//! `poll_interval` fallback when idle. Chosen over semaphore/mpsc because
//! durability already lives in the DB row and plain polling survives a restart
//! with zero extra state (PATTERNS A4 "polling fallback").
//!
//! Semantics locked by Phase 2 context:
//! - **D-04:** the pool size comes ONLY from [`WorkerConfig::max_concurrency`]
//!   (default 1) — never unbounded fan-out. Each worker holds at most one job
//!   in flight, so provider concurrency is bounded by N.
//! - **D-11:** every in-flight job gets a heartbeat refreshing `claimed_at`
//!   every `heartbeat_interval` (default 60 s) while the provider call runs;
//!   a worker killed mid-call stops heartbeating and its job is reaped back to
//!   `pending` after `claim_expiry` (default 5 min) and re-claimed elsewhere.
//!   The heartbeat is deliberately NOT replaced by a timeout cap (CONTEXT.md
//!   Specifics forbids it — long local LLM calls must stay possible).
//! - **D-09/D-10:** provider errors route through `fail_job` (backoff ladder /
//!   dead-letter live in the store); successes through `complete_job`.
//!
//! Heartbeat implementation note: the heartbeat runs as a `tokio::select!`
//! branch *inside* the worker task, not as a detached `tokio::spawn` child —
//! a detached child would survive `JoinHandle::abort()` on the worker, so the
//! kill-9 simulation (task abort == heartbeat stops) could not hold. Dropping
//! the worker task drops the select and stops the heartbeat, exactly like a
//! dead process.
//!
//! Threat model (02-04): `payload_json` is untrusted data — it is placed in
//! the user-message body of the prompt, never in system instructions, and the
//! provider only serde-parses the response. No shell, no eval, no path is
//! derived from payload here. Logs carry job id/kind/error strings only;
//! `CoreError::Llm` Display carries status+url, never key material (D-03).

use std::time::Duration;

use chrono::Utc;
use tokio::task::JoinHandle;

use crate::llm::LlmProvider;
use crate::store::{Job, Store};
use std::sync::Arc;

/// Worker-pool tuning. Durations are injectable so tests run on millisecond
/// scales; the defaults are the D-04/D-11 values verbatim.
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// D-04: pool size — exactly this many claim loops, never more (default 1,
    /// safe for a local iGPU LLM).
    pub max_concurrency: usize,
    /// Idle wait between claim attempts (polling fallback, PATTERNS A4).
    pub poll_interval: Duration,
    /// D-11: `claimed_at` refresh period while a job is in flight (60 s).
    pub heartbeat_interval: Duration,
    /// D-11: a claim older than this without a heartbeat is reaped (5 min).
    pub claim_expiry: Duration,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            max_concurrency: 1, // D-04
            poll_interval: Duration::from_secs(1),
            heartbeat_interval: Duration::from_secs(60), // D-11
            claim_expiry: Duration::from_secs(300),      // D-11 (5 min)
        }
    }
}

/// Spawn exactly `cfg.max_concurrency` worker loops against `store`,
/// executing claimed jobs through `provider` for vault `vault_id`. Returns
/// the join handles for shutdown (plan 02-05 wires the daemon).
pub fn spawn_workers(
    store: Arc<Store>,
    provider: Arc<dyn LlmProvider>,
    vault_id: i64,
    cfg: WorkerConfig,
) -> Vec<JoinHandle<()>> {
    (0..cfg.max_concurrency)
        .map(|i| {
            let store = Arc::clone(&store);
            let provider = Arc::clone(&provider);
            let cfg = cfg.clone();
            tokio::spawn(async move {
                tracing::info!(worker = i, vault_id, "queue worker started");
                worker_loop(store, provider, vault_id, cfg).await;
            })
        })
        .collect()
}

/// One worker: reap → claim(1) → process, forever. A store error is logged and
/// the loop continues after `poll_interval` — one bad iteration never kills
/// the worker (task panics are additionally contained by tokio at the task
/// boundary).
async fn worker_loop(
    store: Arc<Store>,
    provider: Arc<dyn LlmProvider>,
    vault_id: i64,
    cfg: WorkerConfig,
) {
    loop {
        // D-11: a crashed worker's stale claims return to the pool first.
        // (store API takes a chrono TimeDelta — injectable std Duration here.)
        let expiry = chrono::TimeDelta::from_std(cfg.claim_expiry)
            .unwrap_or_else(|_| chrono::TimeDelta::seconds(300));
        if let Err(e) = store
            .reap_expired_claims(vault_id, expiry, Utc::now())
            .await
        {
            tracing::error!(vault_id, "reap_expired_claims failed: {e}");
        }

        let jobs = match store.claim_jobs(vault_id, 1, Utc::now()).await {
            Ok(jobs) => jobs,
            Err(e) => {
                tracing::error!(vault_id, "claim_jobs failed: {e}");
                tokio::time::sleep(cfg.poll_interval).await;
                continue;
            }
        };

        let Some(job) = jobs.into_iter().next() else {
            tokio::time::sleep(cfg.poll_interval).await;
            continue;
        };

        process_job(&store, &provider, &cfg, job).await;
    }
}

/// Run one claimed job: heartbeat ‖ provider call, then complete/fail.
async fn process_job(
    store: &Arc<Store>,
    provider: &Arc<dyn LlmProvider>,
    cfg: &WorkerConfig,
    job: Job,
) {
    // Threat model: payload is untrusted DATA in the user-message body, never
    // system instructions; the provider only serde-parses the response.
    let prompt = format!("job kind: {}\ndata:\n{}", job.kind, job.payload_json);
    let schema = serde_json::json!({});

    // D-07: the trait method is NON-generic and returns serde_json::Value —
    // the worker needs no typed surface, so no generate_json_typed here.
    // The heartbeat branch never returns; aborting this task (kill -9
    // simulation) drops both futures, so the lease goes stale with the worker.
    let outcome = tokio::select! {
        result = provider.generate_json(&prompt, &schema) => result,
        _ = heartbeat(Arc::clone(store), job.id, cfg.heartbeat_interval) => {
            unreachable!("heartbeat loop never returns")
        }
    };

    match outcome {
        Ok(_value) => match store.complete_job(job.id, Utc::now()).await {
            Ok(0) => tracing::warn!(
                job_id = job.id,
                "complete_job affected 0 rows — claim was lost (reaped?) mid-call"
            ),
            Ok(_) => tracing::debug!(job_id = job.id, kind = %job.kind, "job done"),
            Err(e) => tracing::error!(job_id = job.id, "complete_job failed: {e}"),
        },
        Err(e) => {
            // D-09/D-10 transitions (backoff ladder, dead-letter) live in fail_job.
            tracing::error!(job_id = job.id, kind = %job.kind, "provider failed: {e}");
            match store.fail_job(job.id, &e.to_string(), Utc::now()).await {
                Ok(outcome) => tracing::debug!(job_id = job.id, ?outcome, "fail_job applied"),
                Err(fe) => tracing::error!(job_id = job.id, "fail_job failed: {fe}"),
            }
        }
    }
}

/// D-11 heartbeat: refresh `claimed_at` every `interval` until the caller's
/// select drops this future. `tokio::time::interval` fires immediately on the
/// first tick — skipped, the claim already stamped `claimed_at`.
async fn heartbeat(store: Arc<Store>, job_id: i64, interval: Duration) {
    let mut tick = tokio::time::interval(interval);
    tick.tick().await; // immediate first tick — nothing to refresh yet
    loop {
        tick.tick().await;
        if let Err(e) = store.touch_claim(job_id, Utc::now()).await {
            tracing::error!(job_id, "heartbeat touch_claim failed: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_config_defaults_are_the_d11_values() {
        let cfg = WorkerConfig::default();
        assert_eq!(cfg.max_concurrency, 1); // D-04
        assert_eq!(cfg.poll_interval, Duration::from_secs(1));
        assert_eq!(cfg.heartbeat_interval, Duration::from_secs(60)); // D-11
        assert_eq!(cfg.claim_expiry, Duration::from_secs(300)); // D-11 (5 min)
    }
}
