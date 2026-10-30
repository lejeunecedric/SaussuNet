//! End-to-end walking skeleton (plan 03-01, PIPE-01): an `ingest_url` job
//! inserted into the durable queue is claimed by a worker, fetches (through
//! the test-only fetcher seam — the SSRF guard legitimately blocks loopback
//! fixture servers), extracts article Markdown, lands a `raw/YYYY-MM-DD-
//! slug.md` file with D-07 frontmatter, and records the sha256 in
//! `sync_state` (D-08 groundwork). Zero real LLM (MockProvider injected),
//! zero real network.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tempfile::TempDir;

use saussunet_core::config::{FileConfig, RuntimeConfig};
use saussunet_core::error::Result as CoreResult;
use saussunet_core::llm::MockProvider;
use saussunet_core::pipeline::fetch::FetchResult;
use saussunet_core::pipeline::{self, Fetcher};
use saussunet_core::queue::{spawn_workers, WorkerConfig};
use saussunet_core::vault::Vault;
use saussunet_core::{boot_open, Store};

const MARKDOWN: &str = "# Hello Post\n\nthe fetched body text\n";

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Stub fetcher: returns a fixed article for any URL (the guard + real fetch
/// are unit-tested in pipeline::fetch; loopback fixture servers are blocked
/// by design, so the fetch step is injected here — plan Task 2 note).
struct StubFetcher;

#[async_trait]
impl Fetcher for StubFetcher {
    async fn fetch(&self, url: &str) -> CoreResult<FetchResult> {
        Ok(FetchResult {
            final_url: url.to_string(),
            markdown: MARKDOWN.to_string(),
            sha256: sha256_hex(MARKDOWN.as_bytes()),
            title: "Hello Post".to_string(),
        })
    }
}

async fn setup() -> (TempDir, i64, Arc<Store>, Arc<Vault>) {
    pipeline::set_fetcher_override(Arc::new(StubFetcher));
    let tmp = TempDir::new().expect("tempdir");
    let toml = format!(
        "[storage]\ndata_dir = \"{}\"\n\n[vault]\nid = 1\nname = \"main\"\n",
        tmp.path().display()
    );
    let cfg: FileConfig = toml::from_str(&toml).expect("config parses");
    let rt: RuntimeConfig = cfg.resolve().expect("resolve");
    let (store, vault) = boot_open(&rt).await.expect("boot");
    (tmp, vault.id, Arc::new(store), Arc::new(vault))
}

fn fast_cfg() -> WorkerConfig {
    WorkerConfig {
        max_concurrency: 1,
        poll_interval: Duration::from_millis(10),
        heartbeat_interval: Duration::from_millis(50),
        claim_expiry: Duration::from_millis(300),
    }
}

async fn wait_for_status(store: &Store, vault_id: i64, job_ids: &[i64], status: &str) {
    for _ in 0..500 {
        let jobs = store.list_jobs(vault_id, Some(status)).await.expect("list");
        if job_ids.iter().all(|id| jobs.iter().any(|j| j.id == *id)) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timeout: jobs {job_ids:?} never reached status '{status}'");
}

fn find_raw_file(vault: &Vault, suffix: &str) -> Option<PathBuf> {
    let raw = vault.root.join("raw");
    let entries = std::fs::read_dir(&raw).ok()?;
    for e in entries.flatten() {
        let p = e.path();
        if p.file_name()
            .map(|n| n.to_string_lossy().ends_with(suffix))
            .unwrap_or(false)
        {
            return Some(p);
        }
    }
    None
}

/// Truth #1: a queued ingest_url job becomes a raw/ file on disk + sync_state.
#[tokio::test]
async fn ingest_url_job_lands_raw_file_and_sync_state() {
    let (_tmp, vault_id, store, vault) = setup().await;
    let url = "https://example.com/hello-post";
    let payload = format!("{{\"url\": \"{url}\", \"source_id\": \"{url}\"}}");
    let job_id = store
        .insert_job(vault_id, pipeline::JOB_KIND_INGEST_URL, &payload)
        .await
        .expect("insert_job");

    let handles = spawn_workers(
        Arc::clone(&store),
        Arc::new(MockProvider::new(vec![])),
        Arc::clone(&vault),
        vault_id,
        fast_cfg(),
    );

    wait_for_status(&store, vault_id, &[job_id], "done").await;
    let done = store.list_jobs(vault_id, Some("done")).await.expect("list");
    assert!(done.iter().any(|j| j.id == job_id));

    // D-07: raw/YYYY-MM-DD-hello-post.md with frontmatter + exact body.
    let path = find_raw_file(&vault, "-hello-post.md").expect("raw file exists");
    let name = path.file_name().unwrap().to_string_lossy().to_string();
    let date = &name[..10];
    assert_eq!(
        date,
        chrono::Utc::now().format("%Y-%m-%d").to_string(),
        "D-07 date prefix"
    );
    let content = std::fs::read_to_string(&path).expect("read raw file");
    assert!(content.contains(&format!("source_url: {url}")));
    assert!(content.contains("ingested: "));
    assert!(content.contains(&format!("sha256: {}", sha256_hex(MARKDOWN.as_bytes()))));
    assert!(content.ends_with(MARKDOWN), "exact body: {content}");

    // D-08: sync_state carries the source sha256 in its own column
    // (migration 0007 — replaces the 03-01 token-column placeholder).
    assert_eq!(
        store
            .get_source_sha256(vault_id, url)
            .await
            .expect("get_source_sha256")
            .as_deref(),
        Some(sha256_hex(MARKDOWN.as_bytes()).as_str())
    );

    for h in handles {
        h.abort();
    }
}

/// Unknown kinds fail loudly and dead-letter after the retry ladder with
/// the error retained (D-10). The ladder gates (30s/2m/10m) are proven at
/// store level in 02-02; here the worker's Err path is proven once and the
/// remaining ladder runs with injected clocks.
#[tokio::test]
async fn unknown_kind_dead_letters_with_error_retained() {
    let (_tmp, vault_id, store, vault) = setup().await;
    let job_id = store
        .insert_job(vault_id, "bogus.kind", "{}")
        .await
        .expect("insert");

    let handles = spawn_workers(
        Arc::clone(&store),
        Arc::new(MockProvider::new(vec![])),
        Arc::clone(&vault),
        vault_id,
        fast_cfg(),
    );

    // Worker attempt 1: unknown kind → fail_job → pending with the error.
    let mut first_error = None;
    for _ in 0..500 {
        let jobs = store.list_jobs(vault_id, None).await.expect("list");
        let job = jobs.iter().find(|j| j.id == job_id).unwrap();
        if job.status == "pending" && job.attempts == 1 {
            first_error = job.error.clone();
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let err = first_error.expect("first failure must retain the error");
    assert!(
        err.contains("unknown job kind"),
        "error must name the unknown kind, got {err}"
    );

    // Drive the rest of the ladder with injected clocks (WR-01 fencing:
    // claim → fail per retry), preserving the same error string.
    let t0 = chrono::Utc::now();
    for (offset, last) in [(31, false), (153, false), (754, true)] {
        let now = t0 + chrono::TimeDelta::seconds(offset);
        let claimed = store.claim_jobs(vault_id, 1, now).await.expect("claim");
        assert_eq!(claimed.len(), 1, "job due at +{offset}s");
        let token = claimed[0].claim_token.clone().unwrap();
        let outcome = store
            .fail_job(job_id, &token, &err, now)
            .await
            .expect("fail_job");
        if last {
            assert_eq!(
                outcome,
                saussunet_core::store::JobOutcome::Dead,
                "4th failure must dead-letter"
            );
        }
    }

    let jobs = store.list_jobs(vault_id, Some("dead")).await.expect("list");
    let dead = jobs.iter().find(|j| j.id == job_id).expect("dead job");
    assert!(dead
        .error
        .as_deref()
        .is_some_and(|e| e.contains("unknown job kind")));

    for h in handles {
        h.abort();
    }
}
