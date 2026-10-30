//! End-to-end source drift & sha256 no-op (plan 03-04, PIPE-02 / D-08 / D-17):
//! unchanged re-submit = provable zero-LLM no-op; changed source = loud
//! log.md drift entry + re-ingest through the normal merge path, without
//! double-counting the source set. Zero real LLM (Phase 2 D-14); the fetch
//! step rides the 03-01 test seam, keyed off the payload URL so a test can
//! swap served content between runs.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use serde_json::json;
use tempfile::TempDir;

use saussunet_core::config::{FileConfig, RuntimeConfig};
use saussunet_core::error::{CoreError, Result as CoreResult};
use saussunet_core::llm::{LlmProvider, MockProvider, Scripted};
use saussunet_core::pipeline::{self, fetch::FetchResult, Fetcher};
use saussunet_core::store::Job;
use saussunet_core::vault::Vault;
use saussunet_core::{boot_open, Store};

const V1: &str = "# Hello Post\n\nReact is a UI library. It powers many apps.\n";
const V2: &str = "# Hello Post\n\nReact is a UI library with Server Components now. [v2-marker]\n";

/// URL → currently-served markdown, swapped per test between runs.
fn served() -> &'static Mutex<HashMap<String, String>> {
    static MAP: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    MAP.get_or_init(Mutex::default)
}

fn serve(url: &str, markdown: &str) {
    served()
        .lock()
        .unwrap()
        .insert(url.to_string(), markdown.to_string());
}

struct UrlStubFetcher;

#[async_trait]
impl Fetcher for UrlStubFetcher {
    async fn fetch(&self, url: &str) -> CoreResult<FetchResult> {
        let markdown = served()
            .lock()
            .unwrap()
            .get(url)
            .cloned()
            .ok_or_else(|| CoreError::Queue(format!("no stub content for {url}")))?;
        Ok(FetchResult {
            final_url: url.to_string(),
            sha256: sha256_hex(markdown.as_bytes()),
            markdown,
            title: "Drift Post".to_string(),
        })
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

async fn setup() -> (TempDir, i64, Arc<Store>, Arc<Vault>) {
    pipeline::set_fetcher_override(Arc::new(UrlStubFetcher));
    let tmp = TempDir::new().expect("tempdir");
    let toml = format!(
        "[storage]\ndata_dir = \"{}\"\n\n[vault]\nid = 1\nname = \"main\"\n",
        tmp.path().display()
    );
    let cfg: FileConfig = toml::from_str(&toml).expect("config parses");
    let rt: RuntimeConfig = cfg.resolve().expect("resolve");
    let (store, vault) = boot_open(&rt).await.expect("boot");
    // PIPE-06 whitelist: two pre-existing wiki pages the pass-2 fixtures
    // link to, discoverable via index.md stems.
    saussunet_core::vault::index::upsert_entry(&vault, "Concepts", "Meta", "Meta")
        .await
        .expect("index Meta");
    saussunet_core::vault::index::upsert_entry(&vault, "Concepts", "JavaScript", "JavaScript")
        .await
        .expect("index JavaScript");
    (tmp, vault.id, Arc::new(store), Arc::new(vault))
}

/// Drive one full ingest through the public kind-dispatch entry point.
async fn run_ingest(
    store: &Store,
    vault: &Arc<Vault>,
    provider: &Arc<dyn LlmProvider>,
    url: &str,
    source_id: &str,
) {
    let payload = json!({ "url": url, "source_id": source_id }).to_string();
    let job = Job {
        id: 1,
        vault_id: vault.id,
        kind: pipeline::JOB_KIND_INGEST_URL.to_string(),
        payload_json: payload,
        status: "running".to_string(),
        priority: 0,
        attempts: 1,
        error: None,
        created_at: String::new(),
        claimed_at: None,
        finished_at: None,
        claim_token: None,
    };
    pipeline::handle_job(store, vault, provider, &job, "test-claim")
        .await
        .expect("handle_job ingest_url");
}

fn read(vault: &Vault, rel: &str) -> String {
    std::fs::read_to_string(vault.root.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

fn page_exists(vault: &Vault, rel: &str) -> bool {
    vault.root.join(rel).exists()
}

fn today() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

fn raw_rel() -> String {
    format!("raw/{}-drift-post.md", today())
}

/// Pass-1 script: central React + nothing else.
fn pass1_central() -> serde_json::Value {
    json!({
        "candidates": [
            {"name": "React", "kind": "entity", "mentions": 5, "central": true,
             "justification": "the article is about React", "aliases": []}
        ]
    })
}

/// Pass-1 script: non-central mention (sub-threshold on its own).
fn pass1_passing() -> serde_json::Value {
    json!({
        "candidates": [
            {"name": "Rust", "kind": "entity", "mentions": 2, "central": false,
             "justification": "mentioned in passing", "aliases": []}
        ]
    })
}

/// Pass-2 page JSON for `title`, with ≥2 whitelisted wikilinks and a
/// provenance marker to the real raw/ file (PIPE-06 validator passes).
fn page_json(summary: &str) -> serde_json::Value {
    json!({
        "title": "React",
        "page_type": "entity",
        "tags": ["technology"],
        "summary": format!("{summary} ^[{}]", raw_rel()),
        "sections": [{
            "heading": "Overview",
            "markdown": format!("{summary} See [[Meta]] and [[JavaScript]].")
        }],
        "contested": false,
        "contradictions": []
    })
}

fn provider_arc(p: &Arc<MockProvider>) -> Arc<dyn LlmProvider> {
    Arc::clone(p) as Arc<dyn LlmProvider>
}

/// PIPE-02 / ROADMAP SC2: re-submitting an unchanged source completes the
/// job with ZERO LLM calls and zero vault writes (Pitfall 1 token-burn proof).
#[tokio::test]
async fn unchanged_source_is_noop() {
    let (_tmp, _vault_id, store, vault) = setup().await;
    let url = "https://drift.example/unchanged";
    serve(url, V1);

    let p1 = Arc::new(MockProvider::new(vec![
        Scripted::Ok(pass1_central()),
        Scripted::Ok(page_json("React is a UI library.")),
    ]));
    run_ingest(&store, &vault, &provider_arc(&p1), url, url).await;
    assert_eq!(p1.call_count(), 2, "first ingest runs both passes");

    let log_before = read(&vault, "log.md");
    let page_before = read(&vault, "entities/react.md");
    let raw_before = read(&vault, &raw_rel());

    // Fresh provider with an EMPTY script: any LLM call would error out and
    // fail handle_job — call_count == 0 is the token-burn proof.
    let p2 = Arc::new(MockProvider::new(vec![]));
    run_ingest(&store, &vault, &provider_arc(&p2), url, url).await;
    assert_eq!(p2.call_count(), 0, "unchanged source must burn ZERO tokens");

    assert_eq!(read(&vault, "log.md"), log_before, "log.md untouched");
    assert_eq!(
        read(&vault, "entities/react.md"),
        page_before,
        "page bytes untouched"
    );
    assert_eq!(read(&vault, &raw_rel()), raw_before, "raw/ untouched");
}

/// D-08: sha256 changed → raw/ updated, loud log.md drift entry carrying both
/// digests, and the page refreshed through the normal pass-2 merge path.
#[tokio::test]
async fn changed_source_drifts() {
    let (_tmp, _vault_id, store, vault) = setup().await;
    let url = "https://drift.example/changed";
    serve(url, V1);

    let p1 = Arc::new(MockProvider::new(vec![
        Scripted::Ok(pass1_central()),
        Scripted::Ok(page_json("React is a UI library.")),
    ]));
    run_ingest(&store, &vault, &provider_arc(&p1), url, url).await;
    let log_after_first = read(&vault, "log.md");
    assert!(
        !log_after_first.contains("drift |"),
        "first ingest must not log drift"
    );

    serve(url, V2);
    let p2 = Arc::new(MockProvider::new(vec![
        Scripted::Ok(pass1_central()),
        Scripted::Ok(page_json(
            "React is a UI library with Server Components now.",
        )),
    ]));
    run_ingest(&store, &vault, &provider_arc(&p2), url, url).await;
    assert_eq!(p2.call_count(), 2, "drift re-ingest runs both passes");

    let log = read(&vault, "log.md");
    let old_sha = sha256_hex(V1.as_bytes());
    let new_sha = sha256_hex(V2.as_bytes());
    assert!(
        log.contains(&format!("## [{}] drift |", today())),
        "drift entry present: {log}"
    );
    assert!(
        log.contains(&format!("- sha256 {old_sha} -> {new_sha}")),
        "entry carries old -> new digests"
    );
    assert!(
        log.contains(&format!("- re-ingested {}", raw_rel())),
        "entry names the re-ingested raw path"
    );

    assert!(
        read(&vault, &raw_rel()).contains("[v2-marker]"),
        "raw/ file updated to the new content"
    );
    assert!(
        read(&vault, "entities/react.md").contains("Server Components"),
        "page reflects the new content via the merge path"
    );
}

/// D-17 end-to-end: a drift re-ingest of the SAME url cannot add a second
/// source-set row — a sub-threshold entity stays sub-threshold.
#[tokio::test]
async fn drift_does_not_double_count_sources() {
    let (_tmp, vault_id, store, vault) = setup().await;
    let url = "https://drift.example/dcount";
    serve(url, V1);

    let p1 = Arc::new(MockProvider::new(vec![Scripted::Ok(pass1_passing())]));
    run_ingest(&store, &vault, &provider_arc(&p1), url, url).await;
    assert_eq!(
        store.count_entity_sources(vault_id, "Rust").await.unwrap(),
        1
    );

    serve(url, V2);
    let p2 = Arc::new(MockProvider::new(vec![Scripted::Ok(pass1_passing())]));
    run_ingest(&store, &vault, &provider_arc(&p2), url, url).await;

    assert_eq!(
        store.count_entity_sources(vault_id, "Rust").await.unwrap(),
        1,
        "same source_id must never double-count (D-17)"
    );
    assert!(
        !page_exists(&vault, "entities/rust.md"),
        "still below the 2+ threshold after re-ingesting the SAME url"
    );
}

/// First-seen sources behave exactly as before: create entries, no drift line.
#[tokio::test]
async fn first_seen_has_no_drift_entry() {
    let (_tmp, _vault_id, store, vault) = setup().await;
    let url = "https://drift.example/fresh";
    serve(url, V1);

    let p1 = Arc::new(MockProvider::new(vec![
        Scripted::Ok(pass1_central()),
        Scripted::Ok(page_json("React is a UI library.")),
    ]));
    run_ingest(&store, &vault, &provider_arc(&p1), url, url).await;

    let log = read(&vault, "log.md");
    assert!(log.contains("create |"), "create entries present: {log}");
    assert!(!log.contains("drift |"), "no drift line on first ingest");
}
