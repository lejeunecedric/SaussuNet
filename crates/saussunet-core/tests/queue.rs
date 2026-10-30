//! Worker-pool integration tests (plan 02-04, adapted by 03-01): the thin
//! end-to-end slice (insert → claim → handler → done), the D-04 bounded pool,
//! the D-11 heartbeat/reap lease semantics, and the SC3 kill-9 crash-resume
//! proof — all driven with injectable millisecond WorkerConfig durations
//! (zero real 30s/5min waits, zero API keys, D-14).
//!
//! 03-01 replaced the generic provider prompt with kind dispatch
//! (`pipeline::handle_job`), so the slow/hang/delay behaviours that Phase 2
//! scripted into the MockProvider now live behind the test-only fetcher seam
//! (`pipeline::set_fetcher_override`): jobs use kind `ingest_url` and the
//! stub fetcher keys its behaviour off the payload URL. The provider stays
//! an empty MockProvider — the v1 ingest handler never calls it.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{NaiveDateTime, TimeDelta, Utc};
use tempfile::TempDir;

use saussunet_core::config::{FileConfig, RuntimeConfig};
use saussunet_core::error::{CoreError, Result as CoreResult};
use saussunet_core::llm::MockProvider;
use saussunet_core::pipeline::fetch::FetchResult;
use saussunet_core::pipeline::{self, Fetcher};
use saussunet_core::queue::{spawn_workers, WorkerConfig};
use saussunet_core::store::JobOutcome;
use saussunet_core::vault::Vault;
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

async fn setup() -> (TempDir, i64, Arc<Store>, Arc<Vault>) {
    pipeline::set_fetcher_override(Arc::new(StubFetcher));
    let tmp = TempDir::new().expect("tempdir");
    let rt = rt_for(tmp.path(), "main");
    let (store, vault) = boot_open(&rt).await.expect("boot");
    (tmp, vault.id, Arc::new(store), Arc::new(vault))
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

// --- test-only fetcher seam ------------------------------------------------
//
// Behaviour is keyed off the URL so one process-global stub serves every
// test in this binary without cross-test interference:
//   https://stub.test/ok-<uid>          instant success
//   https://stub.test/slow-<uid>        50 ms in flight (pool-bound gauge)
//   https://stub.test/hangfirst-<uid>   FIRST call per URL hangs 10 s, rest ok
//   https://stub.test/hangfail-<uid>    FIRST call per URL sleeps 500 ms, fails
fn payload(url: &str) -> String {
    format!("{{\"url\": \"{url}\", \"source_id\": \"{url}\"}}")
}

fn url_count(url: &str) -> usize {
    CALLS.lock().unwrap().get(url).copied().unwrap_or(0)
}

async fn wait_url_calls(url: &str, n: usize) {
    for _ in 0..500 {
        if url_count(url) >= n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("timeout: fetch calls for {url} never reached {n}");
}

static CALLS: std::sync::LazyLock<Mutex<HashMap<String, usize>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
static SLOW_INFLIGHT: AtomicUsize = AtomicUsize::new(0);
static SLOW_MAX_INFLIGHT: AtomicUsize = AtomicUsize::new(0);
static HANG_FIRST_SEEN: OnceLock<Mutex<HashMap<String, usize>>> = OnceLock::new();

struct StubFetcher;

#[async_trait]
impl Fetcher for StubFetcher {
    async fn fetch(&self, url: &str) -> CoreResult<FetchResult> {
        *CALLS.lock().unwrap().entry(url.to_string()).or_insert(0) += 1;
        let mode = url
            .trim_start_matches("https://stub.test/")
            .split('-')
            .next()
            .unwrap_or("ok")
            .to_string();
        if mode == "slow" {
            let now = SLOW_INFLIGHT.fetch_add(1, Ordering::SeqCst) + 1;
            SLOW_MAX_INFLIGHT.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(50)).await;
            SLOW_INFLIGHT.fetch_sub(1, Ordering::SeqCst);
        } else if mode == "hangfirst" || mode == "hangfail" {
            let seen = HANG_FIRST_SEEN.get_or_init(|| Mutex::new(HashMap::new()));
            let first = {
                let mut g = seen.lock().unwrap();
                *g.entry(url.to_string()).or_insert(0) += 1;
                g[url] == 1
            };
            if first {
                if mode == "hangfail" {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    return Err(CoreError::Fetch {
                        url: url.to_string(),
                        reason: "stub scripted failure".to_string(),
                    });
                }
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
        }
        Ok(FetchResult {
            final_url: url.to_string(),
            markdown: format!("body of {url}"),
            sha256: "stub-sha".to_string(),
            title: String::new(),
        })
    }
}

/// The thin end-to-end slice: insert → claim → pipeline handler → done.
#[tokio::test]
async fn job_flows_through_worker_to_done() {
    let (_tmp, vault_id, store, vault) = setup().await;
    let url = "https://stub.test/ok-done";
    let job_id = store
        .insert_job(vault_id, pipeline::JOB_KIND_INGEST_URL, &payload(url))
        .await
        .expect("insert");

    let mock = Arc::new(MockProvider::new(vec![]));
    let handles = spawn_workers(Arc::clone(&store), mock, vault, vault_id, fast_cfg(1));

    wait_for_status(&store, vault_id, &[job_id], "done").await;

    let (status, _attempts, _error, finished_at) = row(&store, job_id).await;
    assert_eq!(status, "done");
    assert!(finished_at.is_some(), "finished_at must be stamped");
    assert_eq!(url_count(url), 1, "exactly one handler fetch");
    for h in handles {
        h.abort();
    }
}

/// D-04 / LLMP-02: with N=2 workers and 2×N jobs of ~50ms in-flight fetches,
/// the stub's in-flight gauge must reach the bound (real parallelism) and,
/// being a running max, proves it never exceeded N.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pool_is_bounded() {
    let (_tmp, vault_id, store, vault) = setup().await;
    let urls: Vec<String> = (0..4)
        .map(|i| format!("https://stub.test/slow-{i}"))
        .collect();
    let ids: Vec<i64> = {
        let mut v = Vec::new();
        for u in &urls {
            v.push(
                store
                    .insert_job(vault_id, pipeline::JOB_KIND_INGEST_URL, &payload(u))
                    .await
                    .expect("insert"),
            );
        }
        v
    };

    let mock = Arc::new(MockProvider::new(vec![]));
    let handles = spawn_workers(Arc::clone(&store), mock, vault, vault_id, fast_cfg(2));

    wait_for_status(&store, vault_id, &ids, "done").await;

    assert_eq!(
        SLOW_MAX_INFLIGHT.load(Ordering::SeqCst),
        2,
        "pool must reach the bound (2) and never exceed it"
    );
    assert!(urls.iter().all(|u| url_count(u) == 1), "4 fetches total");
    for h in handles {
        h.abort();
    }
}

/// SC3 kill-9 simulation: task abort is the exact observable equivalent of
/// kill -9 — the heartbeat stops and the in-flight handler call is abandoned.
/// The job returns via claim expiry, is re-claimed by a fresh worker, and
/// completes exactly once (no loss, no duplication).
#[tokio::test]
async fn crash_mid_job_resumes_without_duplication() {
    let (_tmp, vault_id, store, vault) = setup().await;
    let url = "https://stub.test/hangfirst-crash";
    let job_id = store
        .insert_job(vault_id, pipeline::JOB_KIND_INGEST_URL, &payload(url))
        .await
        .expect("insert");

    // Worker A hangs 10s (never returns); worker B then succeeds.
    let cfg = fast_cfg(1); // claim expiry 300ms

    let mut handles_a = spawn_workers(
        Arc::clone(&store),
        Arc::new(MockProvider::new(vec![])),
        Arc::clone(&vault),
        vault_id,
        cfg.clone(),
    );
    wait_url_calls(url, 1).await; // A claimed and is hung mid-fetch

    // kill -9: abort drops the worker task AND its heartbeat branch, so the
    // lease goes stale exactly like a dead process.
    for h in handles_a.drain(..) {
        h.abort();
    }

    // Worker B: same store + vault, fresh task. Its reaper returns the stale
    // claim to pending after expiry; it then claims and completes.
    let handles_b = spawn_workers(
        Arc::clone(&store),
        Arc::new(MockProvider::new(vec![])),
        vault,
        vault_id,
        cfg,
    );

    wait_for_status(&store, vault_id, &[job_id], "done").await;

    let (status, attempts, _error, finished_at) = row(&store, job_id).await;
    assert_eq!(status, "done");
    assert!(finished_at.is_some(), "finished_at stamped exactly once");
    assert_eq!(url_count(url), 2, "hang + success, no third execution");
    // Reap preserves attempts and B never failed, so the crash path adds none.
    assert_eq!(
        attempts, 0,
        "reaped claim preserves attempts (no fail_job on abort)"
    );
    for h in handles_b {
        h.abort();
    }
}

/// D-11 heartbeat half: a live (heartbeating) job past the expiry window is
/// NOT reaped; a claimed job without a heartbeat IS reaped by the same call.
#[tokio::test]
async fn heartbeat_keeps_live_job_unreaped() {
    let (_tmp, vault_id, store, vault) = setup().await;
    let url = "https://stub.test/hangfail-hb";
    let job_id = store
        .insert_job(vault_id, pipeline::JOB_KIND_INGEST_URL, &payload(url))
        .await
        .expect("insert");

    let handles = spawn_workers(
        Arc::clone(&store),
        Arc::new(MockProvider::new(vec![])),
        vault,
        vault_id,
        fast_cfg(1), // heartbeat 50ms, expiry 300ms
    );

    wait_url_calls(url, 1).await; // claimed + hung 500ms
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

    // Hang resolves (fails after 500ms) → fail_job backs the job off by the
    // real D-09 30s gate. That gate is already proven at store level (02-02
    // backoff_gates_claim); clear it here so the done path is observable
    // within the test budget.
    wait_for_status(&store, vault_id, &[job_id], "pending").await;
    sqlx::query("UPDATE jobs SET next_attempt_at = NULL WHERE id = ?")
        .bind(job_id)
        .execute(store.pool())
        .await
        .expect("clear backoff gate");

    wait_for_status(&store, vault_id, &[job_id], "done").await;
    assert_eq!(url_count(url), 2, "fail + success");
    for h in handles {
        h.abort();
    }
}

/// D-09/D-10 through the worker without real 30s/120s/600s waits: the
/// worker's Err path must route through fail_job and stamp the first D-09
/// backoff (~now+30s); the ladder to dead-letter is then driven by fail_job
/// with injected clocks (the gate itself is proven at store level in 02-02).
/// Since 03-01 the unknown kind `test.echo` is itself the loud handler error.
#[tokio::test]
async fn provider_failure_routes_through_backoff_then_dead() {
    let (_tmp, vault_id, store, vault) = setup().await;
    let job_id = store
        .insert_job(vault_id, "test.echo", "{\"n\": 0}")
        .await
        .expect("insert");

    let t0 = Utc::now();
    let handles = spawn_workers(
        Arc::clone(&store),
        Arc::new(MockProvider::new(vec![])),
        vault,
        vault_id,
        fast_cfg(1),
    );

    // Phase 1 (through the worker): handler Err → fail_job → pending,
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
    let (_tmp, vault_id, store, vault) = setup().await;
    let urls: Vec<String> = (0..6)
        .map(|i| format!("https://stub.test/ok-conc-{i}"))
        .collect();
    let ids: Vec<i64> = {
        let mut v = Vec::new();
        for u in &urls {
            v.push(
                store
                    .insert_job(vault_id, pipeline::JOB_KIND_INGEST_URL, &payload(u))
                    .await
                    .expect("insert"),
            );
        }
        v
    };

    let handles = spawn_workers(
        Arc::clone(&store),
        Arc::new(MockProvider::new(vec![])),
        vault,
        vault_id,
        fast_cfg(4),
    );

    wait_for_status(&store, vault_id, &ids, "done").await;

    assert!(
        urls.iter().all(|u| url_count(u) == 1),
        "each job executed exactly once"
    );
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
