//! End-to-end two-pass extraction (plan 03-03, PIPE-03/04/05): scripted
//! MockProvider JSON in → real llm-wiki pages on disk. Zero real LLM
//! (Phase 2 D-14); the fetch step rides the 03-01 test seam.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use tempfile::TempDir;

use saussunet_core::config::{FileConfig, RuntimeConfig};
use saussunet_core::error::Result as CoreResult;
use saussunet_core::llm::{LlmProvider, MockProvider, Scripted};
use saussunet_core::pipeline::{self, fetch::FetchResult, Fetcher};
use saussunet_core::store::Job;
use saussunet_core::vault::frontmatter;
use saussunet_core::vault::writer::write_page;
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

fn read_page(vault: &Vault, rel: &str) -> String {
    std::fs::read_to_string(vault.root.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

fn page_exists(vault: &Vault, rel: &str) -> bool {
    vault.root.join(rel).exists()
}

fn today() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

/// The raw/ path `write_raw` produces for the stub fetch (title "Hello Post").
/// PIPE-06: provenance markers must point at this real file.
fn raw_rel() -> String {
    format!("raw/{}-hello-post.md", today())
}

/// Pass-2 page JSON for `title`, with ≥2 whitelisted wikilinks and a
/// provenance marker to the real raw/ file (PIPE-06 validator passes).
fn page_json(title: &str, page_type: &str) -> serde_json::Value {
    json!({
        "title": title,
        "page_type": page_type,
        "tags": ["technology"],
        "summary": format!("{title} is a UI technology. ^[{}]", raw_rel()),
        "sections": [{
            "heading": "Overview",
            "markdown": format!("{title} is maintained widely. ^[{}] See [[Meta]] and [[JavaScript]].", raw_rel())
        }],
        "contested": false,
        "contradictions": []
    })
}

/// PIPE-03 + PIPE-05: a central entity gets a page with sources + provenance;
/// a passing mention (central=false, 1 source < min_sources) gets NO page but
/// its source-set row exists. Exactly 2 provider calls (D-09: two passes).
#[tokio::test]
async fn central_entity_page_created_passing_mention_never_writes_page() {
    let (_tmp, vault_id, store, vault) = setup().await;
    let pass1 = json!({
        "candidates": [
            {"name": "React", "kind": "entity", "mentions": 5, "central": true,
             "justification": "the article is about React", "aliases": ["ReactJS"]},
            {"name": "Web Components", "kind": "concept", "mentions": 1, "central": false,
             "justification": "passing mention", "aliases": []}
        ]
    });
    let provider = Arc::new(MockProvider::new(vec![
        Scripted::Ok(pass1),
        Scripted::Ok(page_json("React", "entity")),
    ]));
    let url = "https://example.com/central";
    run_ingest(&store, &vault, &provider_arc(&provider), url, url).await;

    // Central page on disk, valid llm-wiki shape.
    let content = read_page(&vault, "entities/react.md");
    let (yaml, body) = frontmatter::split(&content);
    let fm = frontmatter::parse(yaml).expect("frontmatter parses");
    let raw_rel = format!("raw/{}-hello-post.md", today());
    assert!(
        fm.sources.iter().any(|s| s == &raw_rel),
        "sources carries the raw/ path: {:?}",
        fm.sources
    );
    assert!(body.contains("^[raw/"), "provenance marker present: {body}");
    assert!(frontmatter::count_outbound_wikilinks(body) >= 2);

    // PIPE-03: passing mention → no page, but source set recorded (D-17).
    assert!(
        !page_exists(&vault, "concepts/web-components.md"),
        "sub-threshold mention must not create a page"
    );
    assert_eq!(
        store
            .count_entity_sources(vault_id, "Web Components")
            .await
            .unwrap(),
        1
    );

    // D-09: exactly pass 1 + one pass 2 — no extra calls.
    assert_eq!(provider.call_count(), 2);
}

/// PIPE-04 (D-15/D-16): a candidate matching an existing alias resolves to
/// the canonical name — ONE page at the canonical slug, and the new alias
/// variant is merged into the registry.
#[tokio::test]
async fn alias_match_resolves_to_canonical_and_merges_new_alias() {
    let (_tmp, vault_id, store, vault) = setup().await;
    store
        .upsert_entity(vault_id, "React", r#"["reactjs"]"#, None, Some("entity"))
        .await
        .unwrap();
    let pass1 = json!({
        "candidates": [
            {"name": "reactjs", "kind": "entity", "mentions": 3, "central": true,
             "justification": "about React", "aliases": ["React.js"]}
        ]
    });
    let provider = Arc::new(MockProvider::new(vec![
        Scripted::Ok(pass1),
        Scripted::Ok(page_json("React", "entity")),
    ]));
    let url = "https://example.com/alias";
    run_ingest(&store, &vault, &provider_arc(&provider), url, url).await;

    assert!(page_exists(&vault, "entities/react.md"));
    assert!(
        !page_exists(&vault, "entities/reactjs.md"),
        "no duplicate page"
    );
    let rec = store
        .resolve_entity(vault_id, "reactjs")
        .await
        .unwrap()
        .expect("resolves");
    assert_eq!(rec.canonical_name, "React");
    let aliases: Vec<String> = serde_json::from_str(&rec.aliases_json).unwrap();
    assert!(
        aliases.contains(&"React.js".to_string()),
        "D-16 merge: {aliases:?}"
    );
    assert_eq!(rec.page_path.as_deref(), Some("entities/react.md"));
}

/// D-17 accumulation: a non-central candidate crosses the threshold only
/// when a SECOND distinct source mentions it — page appears on run 2.
#[tokio::test]
async fn second_distinct_source_crosses_min_sources_threshold() {
    let (_tmp, vault_id, store, vault) = setup().await;
    let pass1 = json!({
        "candidates": [
            {"name": "Rust", "kind": "entity", "mentions": 2, "central": false,
             "justification": "mentioned in passing", "aliases": []}
        ]
    });

    // Run 1: one source → sub-threshold → no page, no pass-2 call.
    let p1 = Arc::new(MockProvider::new(vec![Scripted::Ok(pass1.clone())]));
    run_ingest(
        &store,
        &vault,
        &provider_arc(&p1),
        "https://a.example/x",
        "src-a",
    )
    .await;
    assert!(!page_exists(&vault, "entities/rust.md"));
    assert_eq!(p1.call_count(), 1, "no pass-2 call below threshold");
    assert_eq!(
        store.count_entity_sources(vault_id, "Rust").await.unwrap(),
        1
    );

    // Run 2: second distinct source → 2 >= min_sources → page created.
    let p2 = Arc::new(MockProvider::new(vec![
        Scripted::Ok(pass1),
        Scripted::Ok(page_json("Rust", "entity")),
    ]));
    run_ingest(
        &store,
        &vault,
        &provider_arc(&p2),
        "https://b.example/y",
        "src-b",
    )
    .await;
    assert!(page_exists(&vault, "entities/rust.md"));
    assert_eq!(
        store.count_entity_sources(vault_id, "Rust").await.unwrap(),
        2
    );
}

/// D-12 / PIPE-05: pass 2 reports a contradiction → written page carries
/// `contested: true` + contradictions, keeps BOTH claims, and merges the
/// existing sources list with the new raw/ path.
#[tokio::test]
async fn contradiction_keeps_both_claims_and_sets_contested() {
    let (_tmp, _vault_id, store, vault) = setup().await;
    // The old source must exist under raw/ — PIPE-06 validates source files.
    std::fs::write(vault.root().join("raw/old.md"), "# old source\n").expect("raw/old.md");
    // Pre-existing agent-written page (gen_hash recorded by write_page).
    let existing = "---\ntitle: React\ncreated: 2020-01-01\nupdated: 2020-01-01\n\
                    type: entity\ntags: [technology]\nsources: [raw/old.md]\n---\n\n\
                    React was created by Jordan. See [[JavaScript]] and [[Meta]].\n";
    write_page(&vault, &store, "entities/react.md", existing)
        .await
        .expect("seed page");

    let pass1 = json!({
        "candidates": [
            {"name": "React", "kind": "entity", "mentions": 4, "central": true,
             "justification": "about React", "aliases": []}
        ]
    });
    let merged = json!({
        "title": "React",
        "page_type": "entity",
        "tags": ["technology"],
        "summary": "React is a UI library.",
        "sections": [{
            "heading": "Origin",
            "markdown": format!("React was created by Jordan. See [[JavaScript]] and [[Meta]]. \
                          React was created by Chad. ^[{}]", raw_rel())
        }],
        "contested": true,
        "contradictions": ["origin of React"]
    });
    let provider = Arc::new(MockProvider::new(vec![
        Scripted::Ok(pass1),
        Scripted::Ok(merged),
    ]));
    let url = "https://example.com/contested";
    run_ingest(&store, &vault, &provider_arc(&provider), url, url).await;

    let content = read_page(&vault, "entities/react.md");
    let (yaml, body) = frontmatter::split(&content);
    let fm = frontmatter::parse(yaml).expect("parses");
    assert_eq!(fm.contested, Some(true), "D-12 contested flag");
    assert_eq!(
        fm.contradictions,
        Some(vec!["origin of React".to_string()]),
        "contradictions list retained"
    );
    assert!(body.contains("created by Jordan"), "old claim kept");
    assert!(body.contains("created by Chad"), "new claim kept");
    let raw_rel = format!("raw/{}-hello-post.md", today());
    assert!(
        fm.sources.iter().any(|s| s == "raw/old.md"),
        "old source kept"
    );
    assert!(
        fm.sources.iter().any(|s| s == &raw_rel),
        "new source merged"
    );
}

/// PIPE-06 / ROADMAP SC5: a poisoned pass-2 output with a hallucinated
/// wikilink NEVER reaches disk — the deterministic validator rejects it, the
/// rejection is recorded as a `validation_rejected` proposal for Phase 5
/// review, the job still completes, and the sibling page is still written.
#[tokio::test]
async fn hallucinated_link_page_never_reaches_disk_and_is_recorded() {
    let (_tmp, vault_id, store, vault) = setup().await;
    let pass1 = json!({
        "candidates": [
            {"name": "React", "kind": "entity", "mentions": 5, "central": true,
             "justification": "about React", "aliases": []},
            {"name": "Rust", "kind": "entity", "mentions": 4, "central": true,
             "justification": "also central", "aliases": []}
        ]
    });
    let mut poisoned = page_json("Rust", "entity");
    poisoned["sections"][0]["markdown"] = json!(format!(
        "Rust is a systems language. ^[{}] See [[Dangling Link]] and [[Meta]].",
        raw_rel()
    ));
    let provider = Arc::new(MockProvider::new(vec![
        Scripted::Ok(pass1),
        Scripted::Ok(page_json("React", "entity")),
        Scripted::Ok(poisoned),
    ]));
    // run_ingest asserts handle_job returns Ok → the job completes despite
    // the rejected page (one bad page never fails the whole job).
    run_ingest(
        &store,
        &vault,
        &provider_arc(&provider),
        "https://example.com/poison",
        "src-p",
    )
    .await;

    // Sibling page written; poisoned page absent from disk (SC5).
    assert!(
        page_exists(&vault, "entities/react.md"),
        "valid sibling still written"
    );
    assert!(
        !page_exists(&vault, "entities/rust.md"),
        "poisoned page must NEVER reach disk"
    );

    // Rejection recorded for the Phase 5 review queue, naming the target.
    let proposals = store.list_pending_proposals(vault_id).await.unwrap();
    let rejected: Vec<_> = proposals
        .iter()
        .filter(|p| p.reason == "validation_rejected")
        .collect();
    assert_eq!(
        rejected.len(),
        1,
        "exactly one validation rejection: {proposals:?}"
    );
    assert_eq!(rejected[0].target_path, "entities/rust.md");
    assert!(
        rejected[0].proposed_content.contains("Dangling Link"),
        "proposal keeps the rejected content for review"
    );
}

/// Arc<MockProvider> → Arc<dyn LlmProvider> without repeating the cast.
fn provider_arc(p: &Arc<MockProvider>) -> Arc<dyn LlmProvider> {
    Arc::clone(p) as Arc<dyn LlmProvider>
}

/// UAT-03 live finding: small models routinely ignore the "≥2 [[wikilinks]]"
/// instruction, so every page died at the validator (`requires at least 2
/// outbound [[wikilinks]]`). The pipeline now appends a deterministic Related
/// floor from the entity registry so a compliant page still ships.
#[tokio::test]
async fn wikilink_floor_rescues_page_without_links() {
    let (_tmp, vault_id, store, vault) = setup().await;
    // Registry neighbours the floor can link (whitelist = index ∪ registry).
    store
        .upsert_entity(
            vault_id,
            "Serde",
            "[]",
            Some("entities/serde.md"),
            Some("entity"),
        )
        .await
        .unwrap();
    store
        .upsert_entity(
            vault_id,
            "Cargo",
            "[]",
            Some("entities/cargo.md"),
            Some("entity"),
        )
        .await
        .unwrap();

    let pass1 = json!({
        "candidates": [
            {"name": "Rust", "kind": "entity", "mentions": 9, "central": true,
             "justification": "the source is about Rust", "aliases": []}
        ]
    });
    // Pass-2 output with ZERO wikilinks — the exact live failure shape.
    let bare = json!({
        "title": "Rust",
        "page_type": "entity",
        "tags": ["technology"],
        "summary": format!("Rust is a systems language. ^[{}]", raw_rel()),
        "sections": [{
            "heading": "Overview",
            "markdown": format!("Rust emphasizes safety. ^[{}]", raw_rel())
        }],
        "contested": false,
        "contradictions": []
    });
    let provider = Arc::new(MockProvider::new(vec![
        Scripted::Ok(pass1),
        Scripted::Ok(bare),
    ]));
    let url = "https://example.com/wikilink-floor";
    run_ingest(&store, &vault, &provider_arc(&provider), url, url).await;

    let content = read_page(&vault, "entities/rust.md");
    let (_yaml, body) = frontmatter::split(&content);
    assert!(
        frontmatter::count_outbound_wikilinks(body) >= 2,
        "floor must lift the page to the 2-link validator floor: {body}"
    );
    assert!(
        body.contains("## Related"),
        "floor is a Related section: {body}"
    );
}
