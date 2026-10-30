//! Threshold matrix e2e (plan 03-07, PIPE-03 / D-10/D-14/D-17): pages exist
//! iff a candidate is central to one source OR present in >= min_sources
//! distinct sources; passing mentions NEVER create pages but still record
//! their source-set row; a drift re-ingest never increments the count.
//! Zero real LLM (Phase 2 D-14); the fetch step rides the 03-01 test seam,
//! URL-keyed so a test can swap served content (03-04 drift pattern).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use serde_json::json;
use tempfile::TempDir;

use saussunet_core::config::{FileConfig, RuntimeConfig};
use saussunet_core::error::{CoreError, Result as CoreResult};
use saussunet_core::llm::{LlmProvider, MockProvider, Scripted};
use saussunet_core::pipeline::{self, fetch::FetchResult, pages, schemas, Fetcher};
use saussunet_core::store::Job;
use saussunet_core::vault::Vault;
use saussunet_core::{boot_open, Store};

const V1: &str = "# Threshold Post\n\nRust is a systems language. It powers many tools.\n";
const V2: &str = "# Threshold Post\n\nRust is a systems language with new edits. [v2-marker]\n";

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
            title: "Threshold Post".to_string(),
        })
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Boot a vault from a raw config TOML (so a test can pin `[pipeline]`).
async fn setup_with_config(
    toml_extra: &str,
) -> (TempDir, i64, Arc<Store>, Arc<Vault>, RuntimeConfig) {
    pipeline::set_fetcher_override(Arc::new(UrlStubFetcher));
    let tmp = TempDir::new().expect("tempdir");
    let toml = format!(
        "[storage]\ndata_dir = \"{}\"\n\n[vault]\nid = 1\nname = \"main\"\n{toml_extra}",
        tmp.path().display()
    );
    let cfg: FileConfig = toml::from_str(&toml).expect("config parses");
    let rt: RuntimeConfig = cfg.resolve().expect("resolve");
    let (store, vault) = boot_open(&rt).await.expect("boot");
    // PIPE-06 whitelist: pass-2 fixtures link to these via index.md stems.
    saussunet_core::vault::index::upsert_entry(&vault, "Concepts", "Meta", "Meta")
        .await
        .expect("index Meta");
    saussunet_core::vault::index::upsert_entry(&vault, "Concepts", "JavaScript", "JavaScript")
        .await
        .expect("index JavaScript");
    (tmp, vault.id, Arc::new(store), Arc::new(vault), rt)
}

async fn setup() -> (TempDir, i64, Arc<Store>, Arc<Vault>) {
    let (tmp, id, store, vault, _rt) = setup_with_config("").await;
    (tmp, id, store, vault)
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

fn page_exists(vault: &Vault, rel: &str) -> bool {
    vault.root.join(rel).exists()
}

/// Every generated page file under the vault's page directories — the
/// "no page file ANYWHERE" assertion for sub-threshold candidates.
fn page_files(vault: &Vault) -> Vec<String> {
    let mut out = Vec::new();
    for dir in ["entities", "concepts", "comparisons", "queries"] {
        let entries = match std::fs::read_dir(vault.root.join(dir)) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            if entry.path().extension().is_some_and(|e| e == "md") {
                out.push(format!("{}/{}", dir, entry.file_name().to_string_lossy()));
            }
        }
    }
    out
}

fn today() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

fn raw_rel() -> String {
    format!("raw/{}-threshold-post.md", today())
}

/// Pass-1 script: one non-central single-mention candidate.
fn pass1_passing(name: &str, kind: &str) -> serde_json::Value {
    json!({
        "candidates": [
            {"name": name, "kind": kind, "mentions": 1, "central": false,
             "justification": "mentioned in passing", "aliases": []}
        ]
    })
}

/// Pass-1 script: one central candidate.
fn pass1_central(name: &str, kind: &str) -> serde_json::Value {
    json!({
        "candidates": [
            {"name": name, "kind": kind, "mentions": 5, "central": true,
             "justification": format!("the article is about {name}"), "aliases": []}
        ]
    })
}

/// Pass-2 page JSON for `title`, with ≥2 whitelisted wikilinks and a
/// provenance marker to the real raw/ file (PIPE-06 validator passes).
fn page_json(title: &str, page_type: &str) -> serde_json::Value {
    json!({
        "title": title,
        "page_type": page_type,
        "tags": ["technology"],
        "summary": format!("{title} is a technology. ^[{}]", raw_rel()),
        "sections": [{
            "heading": "Overview",
            "markdown": format!("{title} is used widely. ^[{}] See [[Meta]] and [[JavaScript]].", raw_rel())
        }],
        "contested": false,
        "contradictions": []
    })
}

fn provider_arc(p: &Arc<MockProvider>) -> Arc<dyn LlmProvider> {
    Arc::clone(p) as Arc<dyn LlmProvider>
}

/// PIPE-03: a passing mention (not central, 1 source < min_sources) NEVER
/// creates a page — but its source-set row is recorded (D-17) and pass 2 is
/// never invoked for it (cost discipline: call_count == 1).
#[tokio::test]
async fn passing_mention_never_creates_page() {
    let (_tmp, vault_id, store, vault) = setup().await;
    let url = "https://thr.example/passing";
    serve(url, V1);

    let provider = Arc::new(MockProvider::new(vec![Scripted::Ok(pass1_passing(
        "Side Note",
        "concept",
    ))]));
    run_ingest(&store, &vault, &provider_arc(&provider), url, url).await;

    assert!(
        page_files(&vault).is_empty(),
        "passing mention must not create a page anywhere: {:?}",
        page_files(&vault)
    );
    assert_eq!(
        store
            .count_entity_sources(vault_id, "Side Note")
            .await
            .unwrap(),
        1,
        "source-set row still recorded for sub-threshold candidates (D-17)"
    );
    assert!(
        store
            .list_entities(vault_id)
            .await
            .unwrap()
            .iter()
            .any(|e| e.canonical_name == "Side Note"),
        "registry row exists even though no page was written"
    );
    assert_eq!(
        provider.call_count(),
        1,
        "pass 2 never invoked sub-threshold"
    );
}

/// D-10: centrality alone crosses the gate — ONE source is enough.
#[tokio::test]
async fn central_to_one_creates_page() {
    let (_tmp, _vault_id, store, vault) = setup().await;
    let url = "https://thr.example/central";
    serve(url, V1);

    let provider = Arc::new(MockProvider::new(vec![
        Scripted::Ok(pass1_central("React", "entity")),
        Scripted::Ok(page_json("React", "entity")),
    ]));
    run_ingest(&store, &vault, &provider_arc(&provider), url, url).await;

    assert!(
        page_exists(&vault, "entities/react.md"),
        "central candidate gets a page from ONE source (D-10)"
    );
}

/// D-17 accumulation: two distinct sources, neither central, cross the
/// 2-source gate — the page appears only on the SECOND ingest.
#[tokio::test]
async fn accumulation_crosses_threshold() {
    let (_tmp, vault_id, store, vault) = setup().await;
    let url_a = "https://thr.example/acc-a";
    let url_b = "https://thr.example/acc-b";
    serve(url_a, V1);
    serve(url_b, V1);

    // Source A: non-central → sub-threshold → no page, no pass-2 call.
    let p1 = Arc::new(MockProvider::new(vec![Scripted::Ok(pass1_passing(
        "Rust", "entity",
    ))]));
    run_ingest(&store, &vault, &provider_arc(&p1), url_a, "src-a").await;
    assert!(!page_exists(&vault, "entities/rust.md"));
    assert_eq!(p1.call_count(), 1);

    // Source B: second DISTINCT source → 2 >= min_sources → page NOW exists.
    let p2 = Arc::new(MockProvider::new(vec![
        Scripted::Ok(pass1_passing("Rust", "entity")),
        Scripted::Ok(page_json("Rust", "entity")),
    ]));
    run_ingest(&store, &vault, &provider_arc(&p2), url_b, "src-b").await;
    assert!(
        page_exists(&vault, "entities/rust.md"),
        "second distinct source must cross the threshold (D-17)"
    );
    assert_eq!(
        store.count_entity_sources(vault_id, "Rust").await.unwrap(),
        2
    );
}

/// D-17 page-level consequence (03-04 proved the store half): a drift
/// re-ingest of the SAME url never increments the count, so a sub-threshold
/// entity stays sub-threshold — no page after the drift either.
#[tokio::test]
async fn drift_reingest_no_double_count() {
    let (_tmp, vault_id, store, vault) = setup().await;
    let url = "https://thr.example/drift";
    serve(url, V1);

    let p1 = Arc::new(MockProvider::new(vec![Scripted::Ok(pass1_passing(
        "Rust", "entity",
    ))]));
    run_ingest(&store, &vault, &provider_arc(&p1), url, url).await;
    assert!(!page_exists(&vault, "entities/rust.md"));

    // Same URL drifts (sha256 changed) → re-ingest runs, but the source set
    // must not grow: still 1 distinct source, still NO page.
    serve(url, V2);
    let p2 = Arc::new(MockProvider::new(vec![Scripted::Ok(pass1_passing(
        "Rust", "entity",
    ))]));
    run_ingest(&store, &vault, &provider_arc(&p2), url, url).await;

    assert_eq!(
        store.count_entity_sources(vault_id, "Rust").await.unwrap(),
        1,
        "drift re-ingest must never double-count (D-17)"
    );
    assert!(
        !page_exists(&vault, "entities/rust.md"),
        "still sub-threshold after drift re-ingest of the SAME source"
    );
}

/// D-14: `[pipeline] min_sources` from config.toml is honored by the gate —
/// with min_sources=1 a non-central single-source candidate creates a page.
/// Driven through `pages::gate_and_write` with the RESOLVED config value
/// (`handle_job` still hardcodes DEFAULT_MIN_SOURCES until config plumbing
/// lands in the queue handler signature).
#[tokio::test]
async fn min_sources_configurable() {
    let (_tmp, _vault_id, store, vault, rt) =
        setup_with_config("[pipeline]\nmin_sources = 1\n").await;
    assert_eq!(rt.pipeline_min_sources, 1, "config resolves to 1");

    // gate_and_write validates provenance against a REAL raw/ file.
    std::fs::write(vault.root().join("raw/manual.md"), "# manual source\n").expect("raw/manual.md");
    let pass1: schemas::ExtractCandidates =
        serde_json::from_value(pass1_passing("Rust", "entity")).expect("pass-1 envelope parses");
    let provider = Arc::new(MockProvider::new(vec![Scripted::Ok(json!({
        "title": "Rust",
        "page_type": "entity",
        "tags": ["technology"],
        "summary": "Rust is a systems language. ^[raw/manual.md]".to_string(),
        "sections": [{
            "heading": "Overview",
            "markdown": "Rust is used widely. ^[raw/manual.md] See [[Meta]] and [[JavaScript]].".to_string(),
        }],
        "contested": false,
        "contradictions": []
    }))]));

    let written = pages::gate_and_write(
        &store,
        &vault,
        &provider_arc(&provider),
        rt.pipeline_min_sources,
        "raw/manual.md",
        "src-manual",
        "Rust is a systems language.",
        &pass1,
    )
    .await
    .expect("gate_and_write");

    assert_eq!(
        written.len(),
        1,
        "min_sources=1 lets a single source through"
    );
    assert!(page_exists(&vault, "entities/rust.md"));
}
