//! Plan 03-08: Phase 3 acceptance suite — one test per ROADMAP Phase 3
//! success criterion (sc1..sc5), each driven end-to-end through the REAL
//! queue path: `insert_job` → `spawn_workers(MockProvider)` → poll until
//! done → assert vault state. Zero real LLM (Phase 2 D-14); the fetch step
//! rides the 03-01 URL-keyed test seam (03-04 pattern).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use tempfile::TempDir;

use saussunet_core::config::{FileConfig, RuntimeConfig};
use saussunet_core::error::{CoreError, Result as CoreResult};
use saussunet_core::llm::{LlmProvider, MockProvider, Scripted};
use saussunet_core::pipeline::{self, fetch::FetchResult, Fetcher};
use saussunet_core::queue::{spawn_workers, WorkerConfig};
use saussunet_core::vault::frontmatter;
use saussunet_core::vault::writer::write_page;
use saussunet_core::vault::Vault;
use saussunet_core::{boot_open, Store};

/// PDF fixture shared with pipeline_pdf.rs — text: "Saussure linguistics
/// signifier" (verified with `pdftotext -q -layout`).
const PDF: &[u8] = include_bytes!("fixtures/sample.pdf");

const V1: &str = "# Hello Post\n\nReact is a UI library. It powers many apps.\n";
const V2: &str = "# Hello Post\n\nReact is a UI library with Server Components now. [v2-marker]\n";

/// URL → currently-served markdown (process-global fetcher override, keyed
/// by URL so parallel tests never race — 03-04 lesson).
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
            title: "Hello Post".to_string(),
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
    // PIPE-06 whitelist: pass-2 fixtures wikilink these stems.
    saussunet_core::vault::index::upsert_entry(&vault, "Concepts", "Meta", "Meta")
        .await
        .expect("index Meta");
    saussunet_core::vault::index::upsert_entry(&vault, "Concepts", "JavaScript", "JavaScript")
        .await
        .expect("index JavaScript");
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

fn provider_arc(p: &Arc<MockProvider>) -> Arc<dyn LlmProvider> {
    Arc::clone(p) as Arc<dyn LlmProvider>
}

fn payload(url: &str) -> String {
    json!({ "url": url, "source_id": url }).to_string()
}

/// Poll until `job_id` reaches `done`; panic with the error field on death.
async fn wait_done(store: &Store, vault_id: i64, job_id: i64) {
    for _ in 0..500 {
        let jobs = store.list_jobs(vault_id, None).await.expect("list_jobs");
        if let Some(j) = jobs.iter().find(|j| j.id == job_id) {
            match j.status.as_str() {
                "done" => return,
                "dead" | "failed" => panic!(
                    "job {job_id} {}: {}",
                    j.status,
                    j.error.clone().unwrap_or_default()
                ),
                _ => {}
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timeout waiting for job {job_id} to reach done");
}

/// The real queue path: insert a job, spawn workers around `provider`,
/// wait for done, shut the workers down.
async fn run_via_queue(
    store: &Arc<Store>,
    vault: &Arc<Vault>,
    provider: Arc<MockProvider>,
    kind: &str,
    payload_json: String,
) {
    let job_id = store
        .insert_job(vault.id, kind, &payload_json)
        .await
        .expect("insert_job");
    let handles = spawn_workers(
        Arc::clone(store),
        provider_arc(&provider),
        Arc::clone(vault),
        vault.id,
        fast_cfg(),
    );
    wait_done(store, vault.id, job_id).await;
    for h in handles {
        h.abort();
    }
}

/// Same, for a job already inserted (submit_pdf returns its job id).
async fn run_workers_until_done(
    store: &Arc<Store>,
    vault: &Arc<Vault>,
    provider: Arc<MockProvider>,
    job_id: i64,
) {
    let handles = spawn_workers(
        Arc::clone(store),
        provider_arc(&provider),
        Arc::clone(vault),
        vault.id,
        fast_cfg(),
    );
    wait_done(store, vault.id, job_id).await;
    for h in handles {
        h.abort();
    }
}

fn read(vault: &Vault, rel: &str) -> String {
    std::fs::read_to_string(vault.root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

fn page_exists(vault: &Vault, rel: &str) -> bool {
    vault.root().join(rel).exists()
}

fn today() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

/// raw/ path the stub fetcher's "Hello Post" title produces.
fn raw_rel() -> String {
    format!("raw/{}-hello-post.md", today())
}

/// The derived text file the ingest_pdf worker writes for `sample.pdf`.
fn derived_md_rel() -> String {
    format!("raw/papers/{}-sample.md", today())
}

fn pdftotext_available() -> bool {
    std::process::Command::new("pdftotext")
        .arg("-v")
        .output()
        .is_ok()
}

/// Pass-1 script: React central, "Web Components" a passing mention.
fn pass1_react_central() -> serde_json::Value {
    json!({
        "candidates": [
            {"name": "React", "kind": "entity", "mentions": 5, "central": true,
             "justification": "the article is about React", "aliases": []},
            {"name": "Web Components", "kind": "concept", "mentions": 1, "central": false,
             "justification": "passing mention", "aliases": []}
        ]
    })
}

/// Pass-2 page JSON: ≥2 whitelisted wikilinks + provenance to the real raw/
/// file (PIPE-06 validator passes). `summary` is test-controlled so drift
/// tests can prove the page body refreshed.
fn page_json(title: &str, page_type: &str, summary: &str) -> serde_json::Value {
    json!({
        "title": title,
        "page_type": page_type,
        "tags": ["technology"],
        "summary": format!("{summary} ^[{}]", raw_rel()),
        "sections": [{
            "heading": "Overview",
            "markdown": format!("{title} is maintained widely. ^[{}] See [[Meta]] and [[JavaScript]].", raw_rel())
        }],
        "contested": false,
        "contradictions": []
    })
}

/// ROADMAP SC1: submitting a web URL or PDF lands extracted text under raw/
/// with source_url/ingested/sha256 frontmatter AND creates entity/concept
/// pages. PDF leg self-skips loudly if poppler is absent (03-05 pattern).
#[tokio::test]
async fn sc1_submit_url_and_pdf_produce_pages() {
    let (_tmp, vault_id, store, vault) = setup().await;
    let url = "https://accept.example/sc1";
    serve(url, V1);

    let provider = Arc::new(MockProvider::new(vec![
        Scripted::Ok(pass1_react_central()),
        Scripted::Ok(page_json("React", "entity", "React is a UI library.")),
    ]));
    run_via_queue(
        &store,
        &vault,
        provider,
        pipeline::JOB_KIND_INGEST_URL,
        payload(url),
    )
    .await;

    // raw/ file carries the D-07 frontmatter trio.
    let raw = read(&vault, &raw_rel());
    assert!(
        raw.contains(&format!("source_url: {url}")),
        "source_url frontmatter: {raw}"
    );
    assert!(raw.contains("ingested: "), "ingested frontmatter: {raw}");
    assert!(
        raw.contains(&format!("sha256: {}", sha256_hex(V1.as_bytes()))),
        "sha256 frontmatter: {raw}"
    );
    // Entity page created through the queue path.
    assert!(
        page_exists(&vault, "entities/react.md"),
        "URL ingest created the entity page"
    );

    // --- PDF leg ---
    if !pdftotext_available() {
        eprintln!("SKIP sc1 PDF leg: pdftotext not on PATH (CI requires poppler-utils)");
        return;
    }
    let pass1_pdf = json!({
        "candidates": [
            {"name": "Signifier", "kind": "concept", "mentions": 4, "central": true,
             "justification": "the PDF is about the signifier", "aliases": []}
        ]
    });
    let pdf_page = json!({
        "title": "Signifier",
        "page_type": "concept",
        "tags": ["linguistics"],
        "summary": format!("Signifier is a linguistic concept. ^[{}]", derived_md_rel()),
        "sections": [{
            "heading": "Overview",
            "markdown": format!("Signifier anchors semiotics. ^[{}] See [[Meta]] and [[JavaScript]].", derived_md_rel())
        }],
        "contested": false,
        "contradictions": []
    });
    let (job_id, _) = pipeline::submit_pdf(&store, &vault, PDF, "sample.pdf", 1 << 20)
        .await
        .expect("submit_pdf");
    let provider2 = Arc::new(MockProvider::new(vec![
        Scripted::Ok(pass1_pdf),
        Scripted::Ok(pdf_page),
    ]));
    run_workers_until_done(&store, &vault, provider2, job_id).await;

    // Derived text file with D-07 frontmatter pointing at the raw PDF copy.
    let md = read(&vault, &derived_md_rel());
    assert!(md.contains("Saussure linguistics signifier"), "{md}");
    assert!(md.contains("source_url: "), "source_url frontmatter: {md}");
    assert!(md.contains("ingested: "), "ingested frontmatter: {md}");
    assert!(md.contains("sha256: "), "sha256 frontmatter: {md}");
    assert!(
        page_exists(&vault, "concepts/signifier.md"),
        "PDF ingest created the concept page"
    );
    assert_eq!(
        store.list_jobs(vault_id, None).await.unwrap().len(),
        2,
        "both jobs recorded"
    );
}

/// ROADMAP SC2: re-submitting an unchanged source is a provable zero-LLM
/// no-op (empty MockProvider script = any call would fail the job); a
/// changed source logs drift and refreshes the page.
#[tokio::test]
async fn sc2_unchanged_noop_changed_drift() {
    let (_tmp, _vault_id, store, vault) = setup().await;
    let url = "https://accept.example/sc2";
    serve(url, V1);

    let p1 = Arc::new(MockProvider::new(vec![
        Scripted::Ok(pass1_react_central()),
        Scripted::Ok(page_json("React", "entity", "React is a UI library.")),
    ]));
    run_via_queue(
        &store,
        &vault,
        p1,
        pipeline::JOB_KIND_INGEST_URL,
        payload(url),
    )
    .await;
    let log_before = read(&vault, "log.md");
    let page_before = read(&vault, "entities/react.md");
    let raw_before = read(&vault, &raw_rel());

    // Unchanged re-submit: EMPTY script — zero calls is the token-burn proof.
    let p2 = Arc::new(MockProvider::new(vec![]));
    run_via_queue(
        &store,
        &vault,
        Arc::clone(&p2),
        pipeline::JOB_KIND_INGEST_URL,
        payload(url),
    )
    .await;
    assert_eq!(p2.call_count(), 0, "unchanged source must burn ZERO tokens");
    assert_eq!(read(&vault, "log.md"), log_before, "log.md untouched");
    assert_eq!(
        read(&vault, "entities/react.md"),
        page_before,
        "page bytes untouched"
    );
    assert_eq!(read(&vault, &raw_rel()), raw_before, "raw/ untouched");

    // Changed source: drift entry + refreshed page.
    serve(url, V2);
    let p3 = Arc::new(MockProvider::new(vec![
        Scripted::Ok(pass1_react_central()),
        Scripted::Ok(page_json(
            "React",
            "entity",
            "React is a UI library with Server Components now.",
        )),
    ]));
    run_via_queue(
        &store,
        &vault,
        p3,
        pipeline::JOB_KIND_INGEST_URL,
        payload(url),
    )
    .await;
    let log = read(&vault, "log.md");
    assert!(
        log.contains(&format!("## [{}] drift |", today())),
        "drift entry present: {log}"
    );
    assert!(
        log.contains(&format!(
            "- sha256 {} -> {}",
            sha256_hex(V1.as_bytes()),
            sha256_hex(V2.as_bytes())
        )),
        "drift entry carries old -> new digests"
    );
    assert!(
        read(&vault, &raw_rel()).contains("[v2-marker]"),
        "raw/ updated"
    );
    assert!(
        read(&vault, "entities/react.md").contains("Server Components"),
        "page refreshed via drift re-ingest"
    );
}

/// ROADMAP SC3: passing mentions never create pages; central entities do;
/// alias variants resolve to ONE canonical page (registry dedup).
#[tokio::test]
async fn sc3_thresholds_and_dedup() {
    let (_tmp, vault_id, store, vault) = setup().await;

    // Threshold leg: central → page; passing mention → no page, source row.
    let url_a = "https://accept.example/sc3-a";
    serve(url_a, V1);
    let pa = Arc::new(MockProvider::new(vec![
        Scripted::Ok(pass1_react_central()),
        Scripted::Ok(page_json("React", "entity", "React is a UI library.")),
    ]));
    run_via_queue(
        &store,
        &vault,
        pa,
        pipeline::JOB_KIND_INGEST_URL,
        payload(url_a),
    )
    .await;
    assert!(page_exists(&vault, "entities/react.md"));
    assert!(
        !page_exists(&vault, "concepts/web-components.md"),
        "passing mention must never create a page"
    );
    assert_eq!(
        store
            .count_entity_sources(vault_id, "Web Components")
            .await
            .unwrap(),
        1,
        "sub-threshold candidate still records its source set"
    );

    // Dedup leg: "rustlang" matches the seeded alias → ONE canonical page.
    store
        .upsert_entity(vault_id, "Rust", r#"["rustlang"]"#, None, Some("entity"))
        .await
        .unwrap();
    let url_b = "https://accept.example/sc3-b";
    serve(url_b, V1);
    let pass1_alias = json!({
        "candidates": [
            {"name": "rustlang", "kind": "entity", "mentions": 3, "central": true,
             "justification": "about Rust", "aliases": []}
        ]
    });
    let pb = Arc::new(MockProvider::new(vec![
        Scripted::Ok(pass1_alias),
        Scripted::Ok(page_json("Rust", "entity", "Rust is a systems language.")),
    ]));
    run_via_queue(
        &store,
        &vault,
        pb,
        pipeline::JOB_KIND_INGEST_URL,
        payload(url_b),
    )
    .await;
    assert!(
        page_exists(&vault, "entities/rust.md"),
        "canonical page written"
    );
    assert!(
        !page_exists(&vault, "entities/rustlang.md"),
        "alias variant must not create a near-duplicate page"
    );
    let rec = store
        .resolve_entity(vault_id, "rustlang")
        .await
        .unwrap()
        .expect("alias resolves");
    assert_eq!(rec.canonical_name, "Rust");
}

/// ROADMAP SC4: written pages carry sources frontmatter + `^[raw/...]`
/// markers; a contradiction is flagged contested with BOTH claims kept.
#[tokio::test]
async fn sc4_provenance_and_contested() {
    let (_tmp, _vault_id, store, vault) = setup().await;
    // PIPE-06: provenance/sources must point at REAL raw/ files.
    std::fs::write(vault.root().join("raw/old.md"), "# old source\n").expect("raw/old.md");
    let existing = "---\ntitle: React\ncreated: 2020-01-01\nupdated: 2020-01-01\n\
                    type: entity\ntags: [technology]\nsources: [raw/old.md]\n---\n\n\
                    React was created by Jordan. See [[JavaScript]] and [[Meta]].\n";
    write_page(&vault, &store, "entities/react.md", existing)
        .await
        .expect("seed page");

    let url = "https://accept.example/sc4";
    serve(url, V1);
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
        Scripted::Ok(json!({
            "candidates": [
                {"name": "React", "kind": "entity", "mentions": 4, "central": true,
                 "justification": "about React", "aliases": []}
            ]
        })),
        Scripted::Ok(merged),
    ]));
    run_via_queue(
        &store,
        &vault,
        provider,
        pipeline::JOB_KIND_INGEST_URL,
        payload(url),
    )
    .await;

    let content = read(&vault, "entities/react.md");
    let (yaml, body) = frontmatter::split(&content);
    let fm = frontmatter::parse(yaml).expect("parses");
    assert!(
        fm.sources.iter().any(|s| s == "raw/old.md"),
        "old source kept: {:?}",
        fm.sources
    );
    assert!(
        fm.sources.iter().any(|s| s == &raw_rel()),
        "new raw/ source merged: {:?}",
        fm.sources
    );
    assert!(body.contains("^[raw/"), "provenance marker present: {body}");
    assert_eq!(fm.contested, Some(true), "contradiction flagged contested");
    assert_eq!(
        fm.contradictions,
        Some(vec!["origin of React".to_string()]),
        "contradiction recorded, not auto-resolved"
    );
    assert!(body.contains("created by Jordan"), "old claim kept");
    assert!(body.contains("created by Chad"), "new claim kept");
}

/// ROADMAP SC5: every page reaching disk passed the deterministic validator
/// — a poisoned output (dangling link + missing provenance) never lands, is
/// recorded as a `validation_rejected` proposal, and the valid sibling and
/// the job itself still complete.
#[tokio::test]
async fn sc5_validator_gates_every_page() {
    let (_tmp, vault_id, store, vault) = setup().await;
    let url = "https://accept.example/sc5";
    serve(url, V1);
    let pass1 = json!({
        "candidates": [
            {"name": "React", "kind": "entity", "mentions": 5, "central": true,
             "justification": "about React", "aliases": []},
            {"name": "Rust", "kind": "entity", "mentions": 4, "central": true,
             "justification": "also central", "aliases": []}
        ]
    });
    // Poisoned: dangling wikilink AND no ^[...] provenance marker.
    let poisoned = json!({
        "title": "Rust",
        "page_type": "entity",
        "tags": ["technology"],
        "summary": "Rust is a systems language.",
        "sections": [{
            "heading": "Overview",
            "markdown": "Rust is a systems language. See [[Dangling Link]] and [[Meta]]."
        }],
        "contested": false,
        "contradictions": []
    });
    let provider = Arc::new(MockProvider::new(vec![
        Scripted::Ok(pass1),
        Scripted::Ok(page_json("React", "entity", "React is a UI library.")),
        Scripted::Ok(poisoned),
    ]));
    // Job still completes despite the rejected page.
    run_via_queue(
        &store,
        &vault,
        provider,
        pipeline::JOB_KIND_INGEST_URL,
        payload(url),
    )
    .await;

    assert!(
        page_exists(&vault, "entities/react.md"),
        "valid sibling still written"
    );
    assert!(
        !page_exists(&vault, "entities/rust.md"),
        "validator-rejected page must NEVER reach disk"
    );
    let proposals = store.list_pending_proposals(vault_id).await.unwrap();
    let rejected: Vec<_> = proposals
        .iter()
        .filter(|p| p.reason == "validation_rejected")
        .collect();
    assert_eq!(rejected.len(), 1, "exactly one rejection: {proposals:?}");
    assert_eq!(rejected[0].target_path, "entities/rust.md");
}
