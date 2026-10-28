//! Worker-pool integration tests (plan 02-04): the thin end-to-end slice
//! (insert → claim → provider → done), the D-04 bounded pool, the D-11
//! heartbeat/reap lease semantics, and the SC3 kill-9 crash-resume proof —
//! all driven by the deterministic MockProvider with injectable millisecond
//! WorkerConfig durations (zero real 30s/5min waits, zero API keys, D-14).

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use chrono::{NaiveDateTime, TimeDelta, Utc};
use tempfile::TempDir;

use saussunet_core::config::{FileConfig, RuntimeConfig};
use saussunet_core::llm::{MockProvider, Scripted};
use saussunet_core::queue::{spawn_workers, WorkerConfig};
use saussunet_core::store::JobOutcome;
use saussunet_core::{boot_open, Store};

/// Same file-backed fixture as tests/writer.rs / tests/queue_store.rs
/// (file-backed DB required — the in-memory store pins one shared connection).
fn rt_for(data_dir: &Path, name: &str) -> RuntimeConfig {
    let toml = format!(
        "[storage]\ndata_dir = \"{}\"\n\n[vault]\nid = 1\nname = \"{name}\"\n",
        data_dir.display()
    );
    let cfg: FileConfig = toml::from_str(&toml).expect("test config parses");
    cfg.resolve().expect("resolve")
}

async fn setup() -> (TempDir, i64, Arc<Store>) {
    let tmp = TempDir::new().expect("tempdir");
    let rt = rt_for(tmp.path(), "main");
    let (store, vault) = boot_open(&rt).await.expect("boot");
    (tmp, vault.id, Arc::new(store))
}

/// Millisecond-scale config: poll 10ms, heartbeat 50ms, claim expiry 300ms.
fn fast_cfg(max_concurrency: usize) -> WorkerConfig {
    WorkerConfig {
        max_concurrency,
        poll_interval: Duration::from_millis(10),
        heartbeat_interval: Duration::from_millis(50),
        claim_expiry: Duration::from_millis(300),
    }
}

/// Bounded wait: poll every 10ms (≈5s budget) until every id in `job_ids`
/// carries `status`; panic with context on timeout.
async fn wait_for_status(store: &Store, vault_id: i64, job_ids: &[i64], status: &str) {
    for _ in 0..500 {
        let jobs = store
            .list_jobs(vault_id, Some(status))
            .await
            .expect("list_jobs");
        if job_ids.iter().all(|id| jobs.iter().any(|j| j.id == *id)) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timeout: jobs {job_ids:?} never reached status '{status}'");
}

/// Bounded wait for the mock to have served at least `n` calls.
async fn wait_call_count(mock: &MockProvider, n: usize) {
    for _ in 0..500 {
        if mock.call_count() >= n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("timeout: mock call_count never reached {n}");
}

/// (status, attempts, error, finished_at) for `id`.
async fn row(store: &Store, id: i64) -> (String, i64, Option<String>, Option<String>) {
    let status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id = ?")
        .bind(id)
        .fetch_one(store.pool())
        .await
        .expect("status");
    let attempts: i64 = sqlx::query_scalar("SELECT attempts FROM jobs WHERE id = ?")
        .bind(id)
        .fetch_one(store.pool())
        .await
        .expect("attempts");
    let error: Option<String> = sqlx::query_scalar("SELECT error FROM jobs WHERE id = ?")
        .bind(id)
        .fetch_one(store.pool())
        .await
        .expect("error");
    let finished_at: Option<String> =
        sqlx::query_scalar("SELECT finished_at FROM jobs WHERE id = ?")
            .bind(id)
            .fetch_one(store.pool())
            .await
            .expect("finished_at");
    (status, attempts, error, finished_at)
}

/// Parse a SQLite TEXT timestamp back to UTC.
fn parse_ts(s: &str) -> chrono::DateTime<Utc> {
    NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
        .expect("SQLite timestamp")
        .and_utc()
}

/// The thin end-to-end slice: insert → claim → provider.generate_json → done.
#[tokio::test]
async fn job_flows_through_worker_to_done() {
    let (_tmp, vault_id, store) = setup().await;
    let job_id = store
        .insert_job(vault_id, "test.echo", "{\"n\": 0}")
        .await
        .expect("insert");

    let mock = Arc::new(MockProvider::new(vec![Scripted::Ok(
        serde_json::json!({"ok": true}),
    )]));
    let handles = spawn_workers(Arc::clone(&store), mock.clone(), vault_id, fast_cfg(1));

    wait_for_status(&store, vault_id, &[job_id], "done").await;

    let (status, _attempts, _error, finished_at) = row(&store, job_id).await;
    assert_eq!(status, "done");
    assert!(finished_at.is_some(), "finished_at must be stamped");
    assert_eq!(mock.call_count(), 1, "exactly one provider call");
    for h in handles {
        h.abort();
    }
}

/// D-04 / LLMP-02: with N=2 workers and 2×N jobs of 50ms SlowOk responses,
/// the mock's in-flight gauge must reach the bound (real parallelism) and,
/// being a running max, proves it never exceeded N.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pool_is_bounded() {
    let (_tmp, vault_id, store) = setup().await;
    let ids: Vec<i64> = {
        let mut v = Vec::new();
        for i in 0..4 {
            v.push(
                store
                    .insert_job(vault_id, "test.echo", &format!("{{\"n\": {i}}}"))
                    .await
                    .expect("insert"),
            );
        }
        v
    };

    // Delayed successes so every response is in-flight for ~50ms and the
    // gauge can actually observe overlap (instant Ok would pass trivially).
    let mock = Arc::new(MockProvider::new(
        (0..4)
            .map(|_| Scripted::SlowOk {
                delay: Duration::from_millis(50),
                value: serde_json::json!({"ok": true}),
            })
            .collect(),
    ));
    let handles = spawn_workers(Arc::clone(&store), mock.clone(), vault_id, fast_cfg(2));

    wait_for_status(&store, vault_id, &ids, "done").await;

    assert_eq!(
        mock.max_concurrent_calls(),
        2,
        "pool must reach the bound (2) and never exceed it"
    );
    assert_eq!(mock.call_count(), 4);
    for h in handles {
        h.abort();
    }
}

/// SC3 kill-9 simulation: task abort is the exact observable equivalent of
/// kill -9 — the heartbeat stops and the in-flight call is abandoned. The
/// job returns via claim expiry, is re-claimed by a fresh worker, and
/// completes exactly once (no loss, no duplication).
#[tokio::test]
async fn crash_mid_job_resumes_without_duplication() {
    let (_tmp, vault_id, store) = setup().await;
    let job_id = store
        .insert_job(vault_id, "test.echo", "{\"n\": 0}")
        .await
        .expect("insert");

    // Worker A hangs 10s (never returns); worker B then succeeds.
    let mock = Arc::new(MockProvider::new(vec![
        Scripted::Hang(Duration::from_secs(10)),
        Scripted::Ok(serde_json::json!({"ok": true})),
    ]));
    let cfg = fast_cfg(1); // claim expiry 300ms

    let mut handles_a = spawn_workers(Arc::clone(&store), mock.clone(), vault_id, cfg.clone());
    wait_call_count(&mock, 1).await; // A claimed and is hung mid-call

    // kill -9: abort drops the worker task AND its heartbeat branch, so the
    // lease goes stale exactly like a dead process.
    for h in handles_a.drain(..) {
        h.abort();
    }

    // Worker B: same store + provider, fresh task. Its reaper returns the
    // stale claim to pending after expiry; it then claims and completes.
    let handles_b = spawn_workers(Arc::clone(&store), mock.clone(), vault_id, cfg);

    wait_for_status(&store, vault_id, &[job_id], "done").await;

    let (status, attempts, _error, finished_at) = row(&store, job_id).await;
    assert_eq!(status, "done");
    assert!(finished_at.is_some(), "finished_at stamped exactly once");
    assert_eq!(mock.call_count(), 2, "hang + success, no third execution");
    // Reap preserves attempts and B never failed, so the crash path adds none.
    assert_eq!(
        attempts, 0,
        "reaped claim preserves attempts (no fail_job on abort)"
    );
    assert_eq!(
        mock.remaining().await,
        0,
        "script fully consumed — no extra call"
    );
    for h in handles_b {
        h.abort();
    }
}

/// D-11 heartbeat half: a live (heartbeating) job past the expiry window is
/// NOT reaped; a claimed job without a heartbeat IS reaped by the same call.
#[tokio::test]
async fn heartbeat_keeps_live_job_unreaped() {
    let (_tmp, vault_id, store) = setup().await;
    let job_id = store
        .insert_job(vault_id, "test.echo", "{\"n\": 0}")
        .await
        .expect("insert");

    let mock = Arc::new(MockProvider::new(vec![
        Scripted::Hang(Duration::from_millis(500)),
        Scripted::Ok(serde_json::json!({"ok": true})),
    ]));
    let handles = spawn_workers(
        Arc::clone(&store),
        mock.clone(),
        vault_id,
        fast_cfg(1), // heartbeat 50ms, expiry 300ms
    );

    wait_call_count(&mock, 1).await; // claimed + hung
                                     // Past the expiry window while still hung and heartbeating.
    tokio::time::sleep(Duration::from_millis(400)).await;

    let reaped = store
        .reap_expired_claims(vault_id, TimeDelta::milliseconds(300), Utc::now())
        .await
        .expect("reap");
    assert_eq!(reaped, 0, "fresh heartbeats must defeat the reaper");

    // Control: a claimed job whose heartbeat was never started (claimed_at
    // injected in the past) IS reaped by the same window. Parked in a second
    // vault so the live worker (polling vault 1) never touches it.
    store.ensure_vault(2, "control").await.expect("vault 2");
    let control = store
        .insert_job(2, "test.echo", "{\"n\": 0}")
        .await
        .expect("insert control");
    store
        .claim_jobs(2, 1, Utc::now() - TimeDelta::seconds(10))
        .await
        .expect("stale claim");
    let reaped_control = store
        .reap_expired_claims(2, TimeDelta::milliseconds(300), Utc::now())
        .await
        .expect("reap control");
    assert!(reaped_control > 0, "un-heartbeated stale claim must reap");
    let (control_status, _, _, _) = row(&store, control).await;
    assert_eq!(
        control_status, "pending",
        "reaped control returned to pending"
    );

    // Hang resolves (fails 500) → fail_job backs the job off by the real D-09
    // 30s gate. That gate is already proven at store level (02-02
    // backoff_gates_claim); clear it here so the done path is observable
    // within the test budget.
    wait_for_status(&store, vault_id, &[job_id], "pending").await;
    sqlx::query("UPDATE jobs SET next_attempt_at = NULL WHERE id = ?")
        .bind(job_id)
        .execute(store.pool())
        .await
        .expect("clear backoff gate");

    wait_for_status(&store, vault_id, &[job_id], "done").await;
    assert_eq!(mock.call_count(), 2, "hang + success");
    for h in handles {
        h.abort();
    }
}

/// D-09/D-10 through the worker without real 30s/120s/600s waits: the
/// worker's Err path must route through fail_job and stamp the first D-09
/// backoff (~now+30s); the ladder to dead-letter is then driven by fail_job
/// with injected clocks (the gate itself is proven at store level in 02-02).
#[tokio::test]
async fn provider_failure_routes_through_backoff_then_dead() {
    let (_tmp, vault_id, store) = setup().await;
    let job_id = store
        .insert_job(vault_id, "test.echo", "{\"n\": 0}")
        .await
        .expect("insert");

    let mock = Arc::new(MockProvider::new(vec![Scripted::Fail { status: 500 }]));
    let t0 = Utc::now();
    let handles = spawn_workers(Arc::clone(&store), mock.clone(), vault_id, fast_cfg(1));

    // Phase 1 (through the worker): provider Err → fail_job → pending,
    // attempts=1, next_attempt_at ≈ now + 30s (no real 30s wait).
    for _ in 0..500 {
        let (status, attempts, _, _) = row(&store, job_id).await;
        if status == "pending" && attempts == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let next_attempt_at: Option<String> =
        sqlx::query_scalar("SELECT next_attempt_at FROM jobs WHERE id = ?")
            .bind(job_id)
            .fetch_one(store.pool())
            .await
            .expect("next_attempt_at");
    let next = parse_ts(
        next_attempt_at
            .as_deref()
            .expect("backoff gate must be set"),
    );
    assert!(
        next >= t0 + TimeDelta::seconds(28) && next <= t0 + TimeDelta::seconds(32),
        "first retry gate must be ~now+30s (D-09), got {next:?} vs t0 {t0:?}"
    );

    // Phase 2 (injected clocks): three more failures, each on a fresh claim
    // (WR-01 fencing requires claim → fail per retry) and each past its gate
    // → the 4th failure dead-letters with the error retained (D-10).
    let claim_fail = |store: Arc<Store>, tok_now: chrono::DateTime<Utc>, msg: &'static str| async move {
        let c = store
            .claim_jobs(vault_id, 1, tok_now)
            .await
            .expect("phase-2 claim");
        assert_eq!(c.len(), 1, "job must be due and claimable");
        let token = c[0].claim_token.clone().expect("claim token");
        store
            .fail_job(job_id, &token, "500", tok_now)
            .await
            .expect(msg)
    };
    let outcome = claim_fail(Arc::clone(&store), t0 + TimeDelta::seconds(31), "fail 2").await;
    assert!(matches!(outcome, JobOutcome::Retrying { .. }));
    claim_fail(Arc::clone(&store), t0 + TimeDelta::seconds(153), "fail 3").await;
    let outcome = claim_fail(Arc::clone(&store), t0 + TimeDelta::seconds(754), "fail 4").await;
    assert_eq!(outcome, JobOutcome::Dead);

    let (status, attempts, error, _) = row(&store, job_id).await;
    assert_eq!(status, "dead");
    assert_eq!(attempts, 4);
    assert!(error.as_deref().is_some_and(|e| e.contains("500")));
    for h in handles {
        h.abort();
    }
}

/// D-12 under real pool concurrency: 4 workers over 6 jobs — every job
/// executes exactly once (no duplication, no loss).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_workers_no_duplication() {
    let (_tmp, vault_id, store) = setup().await;
    let ids: Vec<i64> = {
        let mut v = Vec::new();
        for i in 0..6 {
            v.push(
                store
                    .insert_job(vault_id, "test.echo", &format!("{{\"n\": {i}}}"))
                    .await
                    .expect("insert"),
            );
        }
        v
    };

    let mock = Arc::new(MockProvider::new(
        (0..6)
            .map(|_| Scripted::Ok(serde_json::json!({"ok": true})))
            .collect(),
    ));
    let handles = spawn_workers(Arc::clone(&store), mock.clone(), vault_id, fast_cfg(4));

    wait_for_status(&store, vault_id, &ids, "done").await;

    assert_eq!(mock.call_count(), 6, "each job executed exactly once");
    let done = store.list_jobs(vault_id, Some("done")).await.expect("list");
    assert_eq!(done.len(), 6);
    assert!(store
        .list_jobs(vault_id, Some("pending"))
        .await
        .expect("list")
        .is_empty());
    for h in handles {
        h.abort();
    }
}
