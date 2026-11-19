//! Plan 04.1-04 T3 (D-03/D-07): entity subtype end-to-end. Pass-2's
//! `page_type` is accepted verbatim ONLY when it is in the SCHEMA-declared
//! subtype vocabulary; unknown or absent folds deterministically to
//! `Entity` and the pass still SUCCEEDS (tolerant parse, Phase 2 D-05).
//! The folded value also lands in `entity_registry.page_type`, and the
//! first-class pass-2 `description` wins over the first-sentence fallback.
//! Zero real LLM (MockProvider, D-29 harness copied from
//! tests/pipeline_thresholds.rs).

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
use saussunet_core::vault::{frontmatter, okf, Vault};
use saussunet_core::{boot_open, Store};

const V1: &str = "# Subtype Post\n\nAda Lovelace wrote the first algorithm.\n";

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
            title: "Subtype Post".to_string(),
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
    // PIPE-06 whitelist: pass-2 fixtures link to these via index.md stems.
    saussunet_core::vault::index::upsert_entry(&vault, "Concepts", "Meta", "Meta", None)
        .await
        .expect("index Meta");
    saussunet_core::vault::index::upsert_entry(
        &vault,
        "Concepts",
        "JavaScript",
        "JavaScript",
        None,
    )
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

fn today() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

fn raw_rel() -> String {
    format!("raw/{}-subtype-post.md", today())
}

/// Pass-1 script: one central entity candidate.
fn pass1_central(name: &str) -> serde_json::Value {
    json!({
        "candidates": [
            {"name": name, "kind": "entity", "mentions": 5, "central": true,
             "justification": format!("the article is about {name}"), "aliases": []}
        ]
    })
}

/// Pass-2 page JSON with an explicit `page_type` (None = key absent).
/// Carries the first-class `description` (D-07) plus ≥2 whitelisted
/// wikilinks and a provenance marker so the PIPE-06 gate passes.
fn page_json(title: &str, page_type: Option<&str>) -> serde_json::Value {
    let mut v = json!({
        "title": title,
        "tags": ["technology"],
        "description": format!("{title}: a scripted one-line description."),
        "summary": format!("{title} is a technology. ^[{}]", raw_rel()),
        "sections": [{
            "heading": "Overview",
            "markdown": format!("{title} is used widely. ^[{}] See [[Meta]] and [[JavaScript]].", raw_rel())
        }],
        "contested": false,
        "contradictions": []
    });
    if let Some(pt) = page_type {
        v["page_type"] = json!(pt);
    }
    v
}

fn provider_arc(p: &Arc<MockProvider>) -> Arc<dyn LlmProvider> {
    Arc::clone(p) as Arc<dyn LlmProvider>
}

/// Read the on-disk page's frontmatter `type` (and the whole fm for
/// description checks).
fn disk_fm(vault: &Vault, rel: &str) -> frontmatter::PageFrontmatter {
    let content = std::fs::read_to_string(vault.root().join(rel))
        .unwrap_or_else(|e| panic!("{rel} unreadable: {e}"));
    let (yaml, _) = frontmatter::split(&content);
    frontmatter::parse(yaml).unwrap_or_else(|e| panic!("{rel} fm parse: {e}"))
}

async fn registry_page_type(store: &Store, vault_id: i64, name: &str) -> Option<String> {
    store
        .list_entities(vault_id)
        .await
        .unwrap()
        .into_iter()
        .find(|e| e.canonical_name == name)
        .and_then(|e| e.page_type)
}

/// T3 case 1: in-vocabulary subtype emitted VERBATIM on disk and stored in
/// the registry; the pass-2 description wins over the fallback (D-07).
#[tokio::test]
async fn entity_subtype_verbatim_in_vocabulary() {
    let (_tmp, vault_id, store, vault) = setup().await;
    let url = "https://sub.example/person";
    serve(url, V1);

    let provider = Arc::new(MockProvider::new(vec![
        Scripted::Ok(pass1_central("Ada Lovelace")),
        Scripted::Ok(page_json("Ada Lovelace", Some("Person"))),
    ]));
    run_ingest(&store, &vault, &provider_arc(&provider), url, url).await;

    let fm = disk_fm(&vault, "entities/ada-lovelace.md");
    assert_eq!(fm.page_type, "Person", "in-vocabulary subtype verbatim");
    assert_eq!(
        fm.description.as_deref(),
        Some("Ada Lovelace: a scripted one-line description."),
        "D-07: pass-2 description wins over the first-sentence fallback"
    );
    assert_eq!(
        registry_page_type(&store, vault_id, "Ada Lovelace").await,
        Some("Person".to_string()),
        "folded subtype stored in entity_registry.page_type"
    );
}

/// T3 case 2: out-of-vocabulary subtype folds to `Entity` and the pass
/// SUCCEEDS — no error, no validation-rejected proposal.
#[tokio::test]
async fn entity_subtype_unknown_folds_to_entity() {
    let (_tmp, vault_id, store, vault) = setup().await;
    let url = "https://sub.example/alien";
    serve(url, V1);

    let provider = Arc::new(MockProvider::new(vec![
        Scripted::Ok(pass1_central("Ada Lovelace")),
        Scripted::Ok(page_json("Ada Lovelace", Some("Alien"))),
    ]));
    run_ingest(&store, &vault, &provider_arc(&provider), url, url).await;

    let fm = disk_fm(&vault, "entities/ada-lovelace.md");
    assert_eq!(
        fm.page_type, "Entity",
        "unknown subtype must fold to Entity"
    );
    assert_eq!(
        registry_page_type(&store, vault_id, "Ada Lovelace").await,
        Some("Entity".to_string()),
        "folded value (not the hallucination) lands in the registry"
    );
}

/// T3 case 3: absent `page_type` folds to `Entity`, pass still succeeds.
#[tokio::test]
async fn entity_subtype_absent_folds_to_entity() {
    let (_tmp, _vault_id, store, vault) = setup().await;
    let url = "https://sub.example/absent";
    serve(url, V1);

    let provider = Arc::new(MockProvider::new(vec![
        Scripted::Ok(pass1_central("Ada Lovelace")),
        Scripted::Ok(page_json("Ada Lovelace", None)),
    ]));
    run_ingest(&store, &vault, &provider_arc(&provider), url, url).await;

    let fm = disk_fm(&vault, "entities/ada-lovelace.md");
    assert_eq!(fm.page_type, "Entity", "absent subtype must fold to Entity");
}

/// Gate-vocabulary unit: `check_page` accepts `type: Person` ONLY when the
/// SCHEMA-declared subtypes are part of the vocabulary parameter — the
/// accepted set equals the declared contract, never more.
#[test]
fn check_page_accepts_subtype_only_when_declared() {
    let yaml = "type: Person\ntitle: Ada Lovelace\ngenerated:\n  by: \"process:saussunet/0.1.0\"\n  at: \"2026-11-19T00:00:00Z\"\n";
    let v_default = okf::check_page(yaml, "body", &okf::DEFAULT_TYPE_VOCAB);
    assert!(
        v_default.iter().any(|v| v.starts_with("E2")),
        "Person must be rejected without the subtype vocab: {v_default:?}"
    );
    let mut vocab: Vec<&str> = okf::DEFAULT_TYPE_VOCAB.to_vec();
    vocab.push("Person");
    let v_union = okf::check_page(yaml, "body", &vocab);
    assert!(
        v_union.is_empty(),
        "declared subtype must pass: {v_union:?}"
    );
}
