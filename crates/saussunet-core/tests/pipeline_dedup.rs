//! Dedup corpus e2e (plan 03-07, PIPE-04 / D-15/D-16): near-duplicate name
//! variants resolve to ONE canonical page via the upgraded deterministic
//! normalization + the alias-accumulation loop — no fuzzy matching anywhere.
//! Zero real LLM (Phase 2 D-14); the fetch step rides the 03-01 test seam.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use tempfile::TempDir;

use saussunet_core::config::{FileConfig, RuntimeConfig};
use saussunet_core::error::Result as CoreResult;
use saussunet_core::llm::{LlmProvider, MockProvider, Scripted};
use saussunet_core::pipeline::{self, fetch::FetchResult, Fetcher};
use saussunet_core::store::Job;
use saussunet_core::vault::Vault;
use saussunet_core::{boot_open, Store};

const MARKDOWN: &str = "# Hello Post\n\nReact is a UI library. It powers many apps.\n";

struct StubFetcher;

#[async_trait]
impl Fetcher for StubFetcher {
    async fn fetch(&self, url: &str) -> CoreResult<FetchResult> {
        Ok(FetchResult {
            final_url: url.to_string(),
            markdown: MARKDOWN.to_string(),
            sha256: "stub-sha".to_string(),
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
    // PIPE-06 whitelist: pass-2 fixtures link to these via index.md stems.
    saussunet_core::vault::index::upsert_entry(&vault, "Concepts", "Meta", "Meta")
        .await
        .expect("index Meta");
    saussunet_core::vault::index::upsert_entry(&vault, "Concepts", "JavaScript", "JavaScript")
        .await
        .expect("index JavaScript");
    (tmp, vault.id, Arc::new(store), Arc::new(vault))
}

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

/// Page file names under entities/ — the "exactly ONE page" assertion.
fn entity_pages(vault: &Vault) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(vault.root.join("entities")) {
        for entry in entries.flatten() {
            if entry.path().extension().is_some_and(|e| e == "md") {
                out.push(entry.file_name().to_string_lossy().to_string());
            }
        }
    }
    out
}

fn today() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

fn raw_rel() -> String {
    format!("raw/{}-hello-post.md", today())
}

/// Pass-1 script: one candidate with the given name/aliases. Non-central so
/// the threshold gate itself stays out of the dedup proof (2 sources cross).
fn pass1(name: &str, aliases: &[&str]) -> serde_json::Value {
    json!({
        "candidates": [
            {"name": name, "kind": "entity", "mentions": 3, "central": false,
             "justification": format!("discusses {name}"), "aliases": aliases}
        ]
    })
}

/// Pass-2 page JSON for `title`, with ≥2 whitelisted wikilinks and a
/// provenance marker to the real raw/ file (PIPE-06 validator passes).
fn page_json(title: &str) -> serde_json::Value {
    json!({
        "title": title,
        "page_type": "entity",
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

/// PIPE-04 / D-15+D-16: three sources naming "React", "reactjs" and
/// "React." (pass-1 alias proposals per D-16) resolve to exactly ONE
/// canonical page and ONE registry row carrying every variant.
#[tokio::test]
async fn alias_variants_one_page() {
    let (_tmp, vault_id, store, vault) = setup().await;

    // Source 1: canonical name teaches its alias variants (D-16 loop).
    let p1 = Arc::new(MockProvider::new(vec![Scripted::Ok(pass1(
        "React",
        &["reactjs", "React.js"],
    ))]));
    run_ingest(
        &store,
        &vault,
        &provider_arc(&p1),
        "https://dedup.example/a",
        "src-a",
    )
    .await;
    assert!(
        entity_pages(&vault).is_empty(),
        "1 source, non-central: no page yet"
    );

    // Source 2: alias variant resolves to the canonical row → 2 sources → page.
    let p2 = Arc::new(MockProvider::new(vec![
        Scripted::Ok(pass1("reactjs", &[])),
        Scripted::Ok(page_json("React")),
    ]));
    run_ingest(
        &store,
        &vault,
        &provider_arc(&p2),
        "https://dedup.example/b",
        "src-b",
    )
    .await;

    // Source 3: punctuated variant normalizes onto the canonical name.
    let p3 = Arc::new(MockProvider::new(vec![
        Scripted::Ok(pass1("React.", &[])),
        Scripted::Ok(page_json("React")),
    ]));
    run_ingest(
        &store,
        &vault,
        &provider_arc(&p3),
        "https://dedup.example/c",
        "src-c",
    )
    .await;

    assert_eq!(
        entity_pages(&vault),
        vec!["react.md".to_string()],
        "exactly ONE page for all name variants (PIPE-04)"
    );
    let rows = store.list_entities(vault_id).await.unwrap();
    assert_eq!(rows.len(), 1, "exactly ONE registry row");
    assert_eq!(rows[0].canonical_name, "React");
    let aliases: Vec<String> = serde_json::from_str(&rows[0].aliases_json).unwrap();
    for variant in ["reactjs", "React.js"] {
        assert!(
            aliases.iter().any(|a| a == variant),
            "aliases_json carries taught variant {variant}: {aliases:?}"
        );
    }
}

/// D-16 accumulation loop: source 1 teaches alias "k8s" for "Kubernetes";
/// a later source whose candidate is bare "k8s" (no alias list) resolves to
/// the canonical row — no second page.
#[tokio::test]
async fn alias_loop_improves() {
    let (_tmp, vault_id, store, vault) = setup().await;

    // Source 1: central Kubernetes, teaches the "k8s" alias.
    let p1 = Arc::new(MockProvider::new(vec![
        Scripted::Ok(json!({
            "candidates": [
                {"name": "Kubernetes", "kind": "entity", "mentions": 5, "central": true,
                 "justification": "the article is about Kubernetes", "aliases": ["k8s"]}
            ]
        })),
        Scripted::Ok(page_json("Kubernetes")),
    ]));
    run_ingest(
        &store,
        &vault,
        &provider_arc(&p1),
        "https://dedup.example/k1",
        "src-k1",
    )
    .await;
    assert!(page_exists(&vault, "entities/kubernetes.md"));

    // Source 3: candidate is the bare alias, no alias list of its own.
    let p3 = Arc::new(MockProvider::new(vec![
        Scripted::Ok(pass1("k8s", &[])),
        Scripted::Ok(page_json("Kubernetes")),
    ]));
    run_ingest(
        &store,
        &vault,
        &provider_arc(&p3),
        "https://dedup.example/k3",
        "src-k3",
    )
    .await;

    assert!(
        !page_exists(&vault, "entities/k8s.md"),
        "alias must resolve, never mint a second page (D-16)"
    );
    assert_eq!(
        entity_pages(&vault),
        vec!["kubernetes.md".to_string()],
        "still exactly one page"
    );
    let rows = store.list_entities(vault_id).await.unwrap();
    assert_eq!(rows.len(), 1, "one registry row after the loop closes");
}

/// Task-1 upgrade proof end-to-end: a simple plural variant of a multi-word
/// name resolves through the upgraded normalize (punctuation + plural fold)
/// to the canonical page — one page, one row.
#[tokio::test]
async fn plural_fold_resolves() {
    let (_tmp, vault_id, store, vault) = setup().await;

    // Source 1: canonical multi-word name with an apostrophe.
    let p1 = Arc::new(MockProvider::new(vec![
        Scripted::Ok(pass1("Fermat's Last Theorem", &[])),
        Scripted::Ok(page_json("Fermat's Last Theorem")),
    ]));
    run_ingest(
        &store,
        &vault,
        &provider_arc(&p1),
        "https://dedup.example/f1",
        "src-f1",
    )
    .await;
    // Non-central single source: no page yet — the proof is about resolution.
    assert!(entity_pages(&vault).is_empty());

    // Source 2: plural variant → normalize folds to the same key → resolves.
    let p2 = Arc::new(MockProvider::new(vec![
        Scripted::Ok(pass1("Fermat's Last Theorems", &[])),
        Scripted::Ok(page_json("Fermat's Last Theorem")),
    ]));
    run_ingest(
        &store,
        &vault,
        &provider_arc(&p2),
        "https://dedup.example/f2",
        "src-f2",
    )
    .await;

    assert_eq!(
        entity_pages(&vault),
        vec!["fermat-s-last-theorem.md".to_string()],
        "plural variant must merge into the canonical page, not fork one"
    );
    let rows = store.list_entities(vault_id).await.unwrap();
    assert_eq!(
        rows.len(),
        1,
        "plural variant must not create a registry row"
    );
    assert_eq!(rows[0].canonical_name, "Fermat's Last Theorem");
}
