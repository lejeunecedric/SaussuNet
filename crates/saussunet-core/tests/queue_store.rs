//! Durable queue-store tests (SRCC-03, D-09..D-12): atomic claim under
//! concurrent workers, D-09 backoff schedule, dead-letter with retained
//! error, claim expiry + heartbeat reap semantics. Every test injects an
//! explicit `now` — zero real-time sleeps (planner decision: SQLite's clock
//! is unreachable from tokio's paused time, so store methods take `now`).

use std::path::Path;
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use tempfile::TempDir;

use saussunet_core::config::{FileConfig, RuntimeConfig};
use saussunet_core::store::JobOutcome;
use saussunet_core::{boot_open, Store};

/// Same boot helper shape as tests/writer.rs (file-backed DB — required for
/// the concurrency test; the in-memory store pins one shared connection).
fn rt_for(data_dir: &Path, name: &str) -> RuntimeConfig {
    let toml = format!(
        "[storage]\ndata_dir = \"{}\"\n\n[vault]\nid = 1\nname = \"{name}\"\n",
        data_dir.display()
    );
    let cfg: FileConfig = toml::from_str(&toml).expect("test config parses");
    cfg.resolve()
}

async fn setup() -> (TempDir, i64, Arc<Store>) {
    let tmp = TempDir::new().expect("tempdir");
    let rt = rt_for(tmp.path(), "main");
    let (store, vault) = boot_open(&rt).await.expect("boot");
    (tmp, vault.id, Arc::new(store))
}

/// Fixed injected clock anchor.
fn t(s: &str) -> DateTime<Utc> {
    s.parse().expect("RFC3339 instant")
}

/// SQLite TEXT format for expected-value comparisons.
fn fmt(dt: DateTime<Utc>) -> String {
    dt.format("%Y-%m-%d %H:%M:%S").to_string()
}

async fn insert_jobs(store: &Store, vault_id: i64, n: usize) -> Vec<i64> {
    let mut ids = Vec::with_capacity(n);
    for i in 0..n {
        ids.push(
            store
                .insert_job(vault_id, "test.echo", &format!("{{\"n\": {i}}}"))
                .await
                .expect("insert"),
        );
    }
    ids
}

/// (status, attempts, error, claimed_at, finished_at, next_attempt_at) for `id`.
async fn row(
    store: &Store,
    id: i64,
) -> (
    String,
    i64,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
) {
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
    let claimed_at: Option<String> = sqlx::query_scalar("SELECT claimed_at FROM jobs WHERE id = ?")
        .bind(id)
        .fetch_one(store.pool())
        .await
        .expect("claimed_at");
    let finished_at: Option<String> =
        sqlx::query_scalar("SELECT finished_at FROM jobs WHERE id = ?")
            .bind(id)
            .fetch_one(store.pool())
            .await
            .expect("finished_at");
    let next_attempt_at: Option<String> =
        sqlx::query_scalar("SELECT next_attempt_at FROM jobs WHERE id = ?")
            .bind(id)
            .fetch_one(store.pool())
            .await
            .expect("next_attempt_at");
    (
        status,
        attempts,
        error,
        claimed_at,
        finished_at,
        next_attempt_at,
    )
}

async fn pending_count(store: &Store, vault_id: i64) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE vault_id = ? AND status = 'pending'")
        .bind(vault_id)
        .fetch_one(store.pool())
        .await
        .expect("count")
}

/// D-12: 8 concurrent claimers over 5 jobs — every job claimed at most
/// once, exactly 5 claimed, 3 left pending.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn claim_is_atomic_under_concurrent_workers() {
    let (_tmp, vault_id, store) = setup().await;
    let ids = insert_jobs(&store, vault_id, 5).await;
    let now = t("2026-10-26T12:00:00Z");

    let mut handles = Vec::new();
    for _ in 0..8 {
        let store = Arc::clone(&store);
        handles.push(tokio::spawn(async move {
            store
                .claim_jobs(vault_id, 1, now)
                .await
                .expect("claim")
                .into_iter()
                .map(|j| j.id)
                .collect::<Vec<_>>()
        }));
    }
    let mut claimed: Vec<i64> = Vec::new();
    for h in handles {
        claimed.extend(h.await.expect("task"));
    }

    assert_eq!(claimed.len(), 5, "exactly 5 jobs claimed");
    let unique: std::collections::HashSet<i64> = claimed.iter().copied().collect();
    assert_eq!(unique.len(), 5, "no job claimed twice");
    assert_eq!(unique, ids.iter().copied().collect());
    // 8 claimers x 1 job over 5 jobs: every job is claimed, none twice, and
    // the 3 losing claimers correctly get nothing (0 pending rows remain).
    // (Plan text said "remaining 3 pending" — arithmetically impossible with
    // these counts; the no-double-claim invariant is what D-12 asserts.)
    assert_eq!(pending_count(&store, vault_id).await, 0);
}

/// D-12 claim order: priority DESC first, then insertion order.
#[tokio::test]
async fn claim_respects_priority_then_created_at() {
    let (_tmp, vault_id, store) = setup().await;
    let ids = insert_jobs(&store, vault_id, 3).await;
    // insert_job keeps its 3-arg signature; bump priority in the fixture.
    sqlx::query("UPDATE jobs SET priority = 10 WHERE id = ?")
        .bind(ids[1])
        .execute(store.pool())
        .await
        .expect("priority bump");
    let now = t("2026-10-26T12:00:00Z");

    let first = store.claim_jobs(vault_id, 1, now).await.expect("claim");
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].id, ids[1], "high-priority job claimed first");

    let second = store.claim_jobs(vault_id, 1, now).await.expect("claim");
    assert_eq!(second[0].id, ids[0], "remaining jobs claimed oldest-first");
}

/// D-09/D-10: retries 1/2/3 → pending with next_attempt_at exactly
/// now+30s / +2min / +10min; the 4th failure dead-letters with the error
/// retained and finished_at set.
#[tokio::test]
#[allow(non_snake_case)] // name pinned by plan acceptance criteria (D-09)
async fn retry_backoff_schedule_matches_D09() {
    let (_tmp, vault_id, store) = setup().await;
    let id = insert_jobs(&store, vault_id, 1).await.remove(0);
    let err = "test-failure-9f3a";

    let t1 = t("2026-10-26T12:00:00Z");
    let out = store.fail_job(id, err, t1).await.expect("fail 1");
    assert_eq!(
        out,
        JobOutcome::Retrying {
            next_attempt_at: fmt(t1 + Duration::seconds(30))
        }
    );
    let (status, attempts, _, _, _, next) = row(&store, id).await;
    assert_eq!((status.as_str(), attempts), ("pending", 1));
    assert_eq!(
        next.as_deref(),
        Some(fmt(t1 + Duration::seconds(30)).as_str())
    );

    let t2 = t1 + Duration::seconds(31);
    let out = store.fail_job(id, err, t2).await.expect("fail 2");
    assert_eq!(
        out,
        JobOutcome::Retrying {
            next_attempt_at: fmt(t2 + Duration::seconds(120))
        }
    );
    let (status, attempts, _, _, _, next) = row(&store, id).await;
    assert_eq!((status.as_str(), attempts), ("pending", 2));
    assert_eq!(
        next.as_deref(),
        Some(fmt(t2 + Duration::seconds(120)).as_str())
    );

    let t3 = t2 + Duration::seconds(121);
    let out = store.fail_job(id, err, t3).await.expect("fail 3");
    assert_eq!(
        out,
        JobOutcome::Retrying {
            next_attempt_at: fmt(t3 + Duration::seconds(600))
        }
    );
    let (status, attempts, _, _, _, next) = row(&store, id).await;
    assert_eq!((status.as_str(), attempts), ("pending", 3));
    assert_eq!(
        next.as_deref(),
        Some(fmt(t3 + Duration::seconds(600)).as_str())
    );

    let t4 = t3 + Duration::seconds(601);
    assert_eq!(
        store.fail_job(id, err, t4).await.expect("fail 4"),
        JobOutcome::Dead
    );
    let (status, attempts, error, _, finished_at, _) = row(&store, id).await;
    assert_eq!((status.as_str(), attempts), ("dead", 4));
    assert_eq!(
        error.as_deref(),
        Some(err),
        "error retained for post-mortem"
    );
    assert_eq!(finished_at.as_deref(), Some(fmt(t4).as_str()));
}

/// A pending job with `next_attempt_at` in the future is NOT claimable;
/// once the due time passes it is.
#[tokio::test]
async fn backoff_gates_claim() {
    let (_tmp, vault_id, store) = setup().await;
    let id = insert_jobs(&store, vault_id, 1).await.remove(0);
    let t0 = t("2026-10-26T12:00:00Z");

    store.claim_jobs(vault_id, 1, t0).await.expect("claim");
    store
        .fail_job(id, "test-failure-9f3a", t0)
        .await
        .expect("fail");

    let early = store
        .claim_jobs(vault_id, 1, t0 + Duration::seconds(10))
        .await
        .expect("claim early");
    assert!(early.is_empty(), "not due yet");

    let due = store
        .claim_jobs(vault_id, 1, t0 + Duration::seconds(31))
        .await
        .expect("claim due");
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].id, id);
}

/// D-11 (expiry half): claims older than the window return to pending with
/// attempts preserved; done and dead rows are never touched.
#[tokio::test]
async fn reap_returns_expired_claims_only() {
    let (_tmp, vault_id, store) = setup().await;
    let ids = insert_jobs(&store, vault_id, 3).await;
    let t0 = t("2026-10-26T12:00:00Z");
    let expiry = Duration::minutes(5);

    // A: live claim. B: done. C: dead (4 failures).
    store.claim_jobs(vault_id, 1, t0).await.expect("claim A");
    store.claim_jobs(vault_id, 1, t0).await.expect("claim B");
    store.complete_job(ids[1], t0).await.expect("complete B");
    store.claim_jobs(vault_id, 1, t0).await.expect("claim C");
    for i in 0..4 {
        store
            .fail_job(ids[2], "test-failure-9f3a", t0 + Duration::seconds(i))
            .await
            .expect("fail C");
    }

    // Too young: nothing reaped.
    let n = store
        .reap_expired_claims(vault_id, expiry, t0 + Duration::minutes(4))
        .await
        .expect("reap early");
    assert_eq!(n, 0);
    assert_eq!(row(&store, ids[0]).await.0, "claimed");

    // Expired: A back to pending, claimed_at cleared, attempts preserved.
    let n = store
        .reap_expired_claims(vault_id, expiry, t0 + Duration::minutes(6))
        .await
        .expect("reap late");
    assert_eq!(n, 1);
    let (status, attempts, _, claimed_at, _, _) = row(&store, ids[0]).await;
    assert_eq!((status.as_str(), attempts), ("pending", 0));
    assert_eq!(claimed_at, None);

    // Done and dead rows untouched.
    assert_eq!(row(&store, ids[1]).await.0, "done");
    assert_eq!(row(&store, ids[2]).await.0, "dead");
}

/// D-11 (heartbeat half): touching the claim refreshes claimed_at so the
/// reaper leaves the job alone.
#[tokio::test]
async fn touch_claim_extends_lease() {
    let (_tmp, vault_id, store) = setup().await;
    let id = insert_jobs(&store, vault_id, 1).await.remove(0);
    let t0 = t("2026-10-26T12:00:00Z");
    let expiry = Duration::minutes(5);

    store.claim_jobs(vault_id, 1, t0).await.expect("claim");
    let touched = store
        .touch_claim(id, t0 + Duration::minutes(4))
        .await
        .expect("touch");
    assert_eq!(touched, 1);
    assert_eq!(
        row(&store, id).await.3.as_deref(),
        Some(fmt(t0 + Duration::minutes(4)).as_str())
    );

    let n = store
        .reap_expired_claims(vault_id, expiry, t0 + Duration::minutes(6))
        .await
        .expect("reap");
    assert_eq!(n, 0, "heartbeat kept the lease alive");
    assert_eq!(row(&store, id).await.0, "claimed");
}

/// claimed → done with finished_at; completing a pending row affects 0 rows.
#[tokio::test]
async fn complete_job_transitions() {
    let (_tmp, vault_id, store) = setup().await;
    let ids = insert_jobs(&store, vault_id, 2).await;
    let t0 = t("2026-10-26T12:00:00Z");

    store.claim_jobs(vault_id, 1, t0).await.expect("claim");
    let n = store.complete_job(ids[0], t0).await.expect("complete");
    assert_eq!(n, 1);
    let (status, _, _, _, finished_at, _) = row(&store, ids[0]).await;
    assert_eq!(status, "done");
    assert_eq!(finished_at.as_deref(), Some(fmt(t0).as_str()));

    // ids[1] is still pending — completing it must be a no-op.
    let n = store
        .complete_job(ids[1], t0)
        .await
        .expect("complete pending");
    assert_eq!(n, 0);
    assert_eq!(row(&store, ids[1]).await.0, "pending");
}
