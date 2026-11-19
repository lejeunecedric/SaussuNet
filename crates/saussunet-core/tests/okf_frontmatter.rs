//! OKF v0.2 conformance gate tests (plan 04.1-01, OKF-01/03/05).
//!
//! T8 `okf_gate_rejects` pins the §11 rule port (`vault::okf::check_page`,
//! the Rust port of B3Wiki `scripts/okflint.py`): one violation per rule
//! class (E1, E2, W2, E5, W1, W3→E), zero on a clean page.
//!
//! T1 `okf_frontmatter_shape`, T5 `sources_entries_shape`,
//! T9 `broken_links_tolerated`, T10 `raw_is_source` pin the write-path
//! rebuild (D-02..D-08, D-13..D-16).

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use serde_json::json;
use tempfile::TempDir;

use saussunet_core::config::{FileConfig, RuntimeConfig};
use saussunet_core::error::{CoreError, Result as CoreResult};
use saussunet_core::llm::{LlmProvider, MockProvider, Scripted};
use saussunet_core::pipeline::{self, fetch::FetchResult, Fetcher};
use saussunet_core::store::Job;
use saussunet_core::vault::{frontmatter, okf, raw, Vault};
use saussunet_core::{boot_open, Store};

const V1: &str = "# Threshold Post\n\nRust is a systems language. It powers many tools.\n";

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

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
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
    saussunet_core::vault::index::upsert_entry(&vault, "Concepts", "Meta", "Meta", None)
        .await
        .expect("index Meta");
    saussunet_core::vault::index::upsert_entry(&vault, "Concepts", "JavaScript", "JavaScript", None)
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

fn today() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

fn raw_rel() -> String {
    format!("raw/{}-threshold-post.md", today())
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

/// Read + parse a vault page from disk.
fn read_page(vault: &Vault, rel: &str) -> (String, frontmatter::PageFrontmatter, String) {
    let content =
        std::fs::read_to_string(vault.root.join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"));
    let (yaml, body) = frontmatter::split(&content);
    let fm = frontmatter::parse(yaml).expect("page frontmatter parses");
    let body = body.to_string();
    (content, fm, body)
}

/// Field of a `generated:` (or sources[] entry) mapping as a string.
fn map_str(v: &serde_yml::Value, k: &str) -> Option<String> {
    match v {
        serde_yml::Value::Mapping(m) => m.get(k).and_then(|x| x.as_str()).map(String::from),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// T1 — a MockProvider-ingested page lands with conformant OKF frontmatter.
// ---------------------------------------------------------------------------
#[tokio::test]
async fn okf_frontmatter_shape() {
    let (_tmp, _id, store, vault) = setup().await;
    let url = "https://okf.example/shape";
    serve(url, V1);
    let provider = Arc::new(MockProvider::new(vec![
        Scripted::Ok(pass1_central("React", "concept")),
        Scripted::Ok(page_json("React", "concept")),
    ]));
    run_ingest(&store, &vault, &provider_arc(&provider), url, url).await;

    let (content, fm, body) = read_page(&vault, "concepts/react.md");
    // D-02: descriptive capitalized type derived from the directory.
    assert_eq!(fm.page_type, "Concept", "page: {content}");
    assert!(!fm.title.trim().is_empty());
    // D-07: never an empty description (first_sentence fallback here).
    assert!(
        !fm.description.as_deref().unwrap_or("").trim().is_empty(),
        "description must be non-empty: {content}"
    );
    // D-01/D-08: offset-bearing timestamps.
    let ts = fm.timestamp.expect("timestamp present");
    assert!(
        okf::iso_offset_ok(&ts),
        "timestamp {ts} must carry an offset"
    );
    let gen = fm.generated.expect("generated present");
    assert_eq!(
        map_str(&gen, "by").as_deref(),
        Some(okf::actor_string().as_str()),
        "D-06 actor"
    );
    let at = map_str(&gen, "at").expect("generated.at");
    assert!(
        okf::iso_offset_ok(&at),
        "generated.at {at} must carry an offset"
    );
    // D-08 single-source pairing: first write — created == updated == today,
    // so timestamp and generated.at derive from the same day and cannot drift.
    assert_eq!(ts, at, "no drift between timestamp and generated.at");
    // D-06 key order on disk.
    let pos = |k: &str| {
        content
            .find(k)
            .unwrap_or_else(|| panic!("missing {k}: {content}"))
    };
    assert!(pos("type:") < pos("title:"));
    assert!(pos("title:") < pos("description:"));
    assert!(pos("description:") < pos("tags:"));
    assert!(pos("tags:") < pos("timestamp:"));
    assert!(pos("timestamp:") < pos("generated:"));
    assert!(pos("generated:") < pos("sources:"));
    // D-13: honest sources[] for the ingested raw record.
    let src = fm.sources.first().expect("sources[] non-empty");
    assert_eq!(
        map_str(src, "resource").as_deref(),
        Some(format!("/{}", raw_rel()).as_str())
    );
    assert_eq!(
        map_str(src, "id").as_deref(),
        Some(&format!("{}-threshold-post", today())[..])
    );
    // MoriNet cross-check (Pitfall 5): generated + sources ⇒ reads as 0.2.
    let (yaml, _) = frontmatter::split(&content);
    assert_eq!(
        okf::check_page(yaml, &body, &okf::DEFAULT_TYPE_VOCAB),
        Vec::<String>::new(),
        "written page must be §11-clean: {content}"
    );
}

// ---------------------------------------------------------------------------
// T5 — sources[] shape: exactly {id, resource, title, last_modified},
// uniq-suffixed ids, first-appearance order, frontmatter provenance listed
// even without inline markers (D-13/D-14).
// ---------------------------------------------------------------------------
#[test]
fn sources_entries_shape_unit() {
    let paths: Vec<String> = vec![
        "raw/2026-10-30-foo.md".into(),
        "raw/articles/2026-10-30-foo.md".into(), // stem collision
        "raw/bar.md".into(),
    ];
    let mut meta = BTreeMap::new();
    meta.insert(
        paths[0].clone(),
        okf::RawSourceMeta {
            title: "Foo".into(),
            last_modified: Some("2026-10-30T12:00:00+00:00".into()),
        },
    );
    meta.insert(
        paths[1].clone(),
        okf::RawSourceMeta {
            title: "Foo2".into(),
            last_modified: None,
        },
    );
    // paths[2] deliberately has no meta — title falls back to the stem.
    let entries = okf::sources_entries(&paths, &meta);
    assert_eq!(entries.len(), 3);
    let keys = |i: usize| -> Vec<String> {
        match &entries[i] {
            serde_yml::Value::Mapping(m) => m.iter().map(|(k, _)| k.to_string()).collect(),
            other => panic!("entry must be a mapping: {other:?}"),
        }
    };
    assert_eq!(keys(0), ["id", "resource", "title", "last_modified"]);
    assert_eq!(map_str(&entries[0], "id"), Some("2026-10-30-foo".into()));
    assert_eq!(
        map_str(&entries[0], "resource"),
        Some("/raw/2026-10-30-foo.md".into())
    );
    // uniq collision suffixing (to_okf.py:68-74).
    assert_eq!(map_str(&entries[1], "id"), Some("2026-10-30-foo-2".into()));
    // No last_modified invented when the raw record is unreadable (honesty).
    assert_eq!(keys(1), ["id", "resource", "title"]);
    assert_eq!(map_str(&entries[2], "title"), Some("bar".into()));
}

#[tokio::test]
async fn sources_entries_shape() {
    let (_tmp, _id, store, vault) = setup().await;
    // Two real raw records; the page cites BOTH in frontmatter but carries an
    // inline marker for only one (D-14: every source still gets listed).
    let fm = raw::RawFrontmatter {
        title: "Alpha".into(),
        source_url: "https://a.example".into(),
        ingested: "2026-10-30T12:00:00+00:00".into(),
        sha256: "aa".into(),
    };
    raw::write_raw(&vault, None, "alpha", "md", &fm, b"a body")
        .await
        .unwrap();
    raw::write_raw(
        &vault,
        None,
        "beta",
        "md",
        &raw::RawFrontmatter {
            title: "Beta".into(),
            source_url: "https://b.example".into(),
            ingested: "2026-10-30T13:00:00+00:00".into(),
            sha256: "bb".into(),
        },
        b"b body",
    )
    .await
    .unwrap();
    let alpha = format!("raw/{}-alpha.md", today());
    let beta = format!("raw/{}-beta.md", today());
    let content = format!(
        "---\ntitle: Union\ntype: concept\ncreated: {}\nupdated: {}\ntags: [technology]\nsources: [{}, {}]\n---\n\nClaim one. ^[{}]\n\nSee [[Meta]] and [[JavaScript]].\n",
        today(),
        today(),
        alpha,
        beta,
        beta
    );
    let rebuilt = okf::rebuild_frontmatter(&vault, &store, "concepts/union.md", &content)
        .await
        .expect("rebuild ok");
    let (yaml, _) = frontmatter::split(&rebuilt);
    let fm = frontmatter::parse(yaml).unwrap();
    let paths: Vec<String> = fm
        .sources
        .iter()
        .map(|s| map_str(s, "resource").unwrap_or_default())
        .collect();
    // First-appearance order: frontmatter order alpha, beta — both listed.
    assert_eq!(paths, vec![format!("/{alpha}"), format!("/{beta}")]);
    assert_eq!(map_str(&fm.sources[0], "title").as_deref(), Some("Alpha"));
    assert_eq!(
        map_str(&fm.sources[0], "last_modified").as_deref(),
        Some("2026-10-30T12:00:00+00:00")
    );
}

// ---------------------------------------------------------------------------
// T9 — broken bundle-relative links are counted, never rejected (D-20).
// ---------------------------------------------------------------------------
#[tokio::test]
async fn broken_links_tolerated() {
    // check_page itself never inspects body links:
    assert_eq!(
        okf::check_page(
            &clean_yaml(),
            "text [Ghost](/never-written.md)",
            &okf::DEFAULT_TYPE_VOCAB
        ),
        Vec::<String>::new()
    );
    // End to end: a generated page linking a never-written markdown target
    // still passes the gate and lands on disk.
    let (_tmp, _id, store, vault) = setup().await;
    let url = "https://okf.example/broken";
    serve(url, V1);
    let mut pj = page_json("Vue", "concept");
    pj["sections"][0]["markdown"] = json!(format!(
        "Vue is used widely. ^[{}] See [[Meta]] and [[JavaScript]], and [Ghost](/ghost-page.md).",
        raw_rel()
    ));
    let provider = Arc::new(MockProvider::new(vec![
        Scripted::Ok(pass1_central("Vue", "concept")),
        Scripted::Ok(pj),
    ]));
    run_ingest(&store, &vault, &provider_arc(&provider), url, url).await;
    let (content, fm, _) = read_page(&vault, "concepts/vue.md");
    assert_eq!(fm.page_type, "Concept", "page: {content}");
}

// ---------------------------------------------------------------------------
// T10 — raw records are OKF `type: Source` concepts (D-15), binaries stay
// frontmatter-free assets (D-16), body bytes stay byte-identical.
// ---------------------------------------------------------------------------
#[tokio::test]
async fn raw_is_source() {
    let tmp = TempDir::new().unwrap();
    let vault = Vault::new(1, "main", tmp.path().join("vault"));
    vault.init_fs().await.unwrap();
    let fm = raw::RawFrontmatter {
        title: "My Post".into(),
        source_url: "https://example.com/post".into(),
        ingested: "2026-10-30T12:00:00+00:00".into(),
        sha256: "abc123".into(),
    };
    let path = raw::write_raw(&vault, None, "post", "md", &fm, b"body bytes")
        .await
        .expect("write_raw");
    let bytes = tokio::fs::read(&path).await.unwrap();
    let content = String::from_utf8(bytes).unwrap();
    let (yaml, body) = frontmatter::split(&content);
    let parsed = frontmatter::parse(yaml).expect("raw frontmatter parses");
    assert_eq!(parsed.page_type, "Source", "D-15: {content}");
    assert_eq!(parsed.title, "My Post");
    let ts = parsed.timestamp.expect("raw timestamp present");
    assert!(okf::iso_offset_ok(&ts), "raw timestamp {ts}");
    let gen = parsed.generated.expect("raw generated present");
    assert_eq!(
        map_str(&gen, "by").as_deref(),
        Some(okf::actor_string().as_str())
    );
    assert!(okf::iso_offset_ok(&map_str(&gen, "at").unwrap()));
    // Extension keys kept verbatim (D-15).
    assert!(content.contains("source_url: https://example.com/post"));
    assert!(content.contains("ingested: 2026-10-30T12:00:00+00:00"));
    assert!(content.contains("sha256: abc123"));
    // Body bytes byte-identical (D-15).
    assert!(body.ends_with("body bytes"), "exact body bytes: {content}");
    // §11-clean as a Source concept.
    assert_eq!(
        okf::check_page(yaml, body, &okf::DEFAULT_TYPE_VOCAB),
        Vec::<String>::new(),
        "raw record must be §11-clean: {content}"
    );

    // D-16: binary raw records carry NO frontmatter.
    let bin = raw::write_raw_binary(&vault, Some("assets"), "doc", "pdf", b"%PDF-1.7\nbinary")
        .await
        .expect("write_raw_binary");
    let bin_bytes = tokio::fs::read(&bin).await.unwrap();
    assert_eq!(bin_bytes, b"%PDF-1.7\nbinary", "binary stays byte-exact");
    assert!(!bin_bytes.starts_with(b"---"), "no frontmatter on assets");
}

/// A fully conformant generated-page frontmatter (D-06 key order, D-08
/// offset-bearing timestamps, D-13 sources shape).
fn clean_yaml() -> String {
    "type: Concept
title: Clean Page
description: A clean page.
tags: [technology]
timestamp: 2026-11-18T10:00:00+00:00
generated:
  by: process:saussunet/0.1.0
  at: 2026-11-18T10:00:00+00:00
sources:
- id: foo
  resource: /raw/foo.md
  title: Foo
  last_modified: 2026-11-18T09:00:00+00:00
"
    .to_string()
}

fn check(fm_yaml: &str) -> Vec<String> {
    okf::check_page(fm_yaml, "", &okf::DEFAULT_TYPE_VOCAB)
}

/// Assert exactly one violation and that it names the expected rule id.
fn one(v: Vec<String>, rule: &str) {
    assert_eq!(
        v.len(),
        1,
        "expected exactly one {rule} violation, got {v:?}"
    );
    assert!(v[0].contains(rule), "violation {v:?} must name rule {rule}");
}

#[test]
fn okf_gate_rejects() {
    let clean = clean_yaml();
    assert_eq!(check(&clean), Vec::<String>::new(), "clean page must pass");

    // E1 — missing / unparseable frontmatter.
    one(check(""), "E1");
    one(check("type: [unclosed\n"), "E1");

    // E2 — empty or non-string `type`.
    one(check(&clean.replace("type: Concept", "type: \"\"")), "E2");
    one(check(&clean.replace("type: Concept", "type: [a, b]")), "E2");

    // W2 — timestamps without an explicit UTC offset (date-only and naive).
    one(
        check(&clean.replace(
            "timestamp: 2026-11-18T10:00:00+00:00",
            "timestamp: 2026-11-18",
        )),
        "W2",
    );
    one(
        check(&clean.replace(
            "timestamp: 2026-11-18T10:00:00+00:00",
            "timestamp: 2026-11-18T10:00:00",
        )),
        "W2",
    );
    // W2 — generated.at without offset.
    one(
        check(&clean.replace("  at: 2026-11-18T10:00:00+00:00", "  at: 2026-11-18")),
        "W2",
    );

    // E5 — `generated` not a mapping / missing `by`.
    one(
        check(&clean.replace(
            "generated:\n  by: process:saussunet/0.1.0\n  at: 2026-11-18T10:00:00+00:00\n",
            "generated: true\n",
        )),
        "E5",
    );
    one(
        check(&clean.replace("  by: process:saussunet/0.1.0\n", "")),
        "E5",
    );

    // W1 — generated.by violating the §7 actor shape.
    one(
        check(&clean.replace("by: process:saussunet/0.1.0", "by: saussu net!")),
        "W1",
    );

    // W3→E — sources[] entry without a resource.
    one(
        check(&clean.replace("  resource: /raw/foo.md\n", "")),
        "W3->E",
    );
}
