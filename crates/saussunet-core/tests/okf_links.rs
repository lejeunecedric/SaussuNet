//! OKF link/citation rewrite tests (plan 04.1-03, OKF-02/OKF-03).
//!
//! T2 `no_wikilinks_on_disk` pins D-09/D-10/D-11: the LLM contract keeps
//! emitting `[[slug]]`, the deterministic writer pre-pass resolves slugs to
//! bundle-relative markdown links, unresolvable slugs stay as broken links,
//! and the ≥2 outbound floor survives on markdown links.
//!
//! Unit cases port B3Wiki `tests/test_to_okf.py::TestWikilinks` (×5) to
//! `okf::rewrite_wikilinks`; the markdown-counter cases mirror
//! `frontmatter::count_outbound_wikilinks` (okflint `MD_LINK` shape).

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
use saussunet_core::vault::{frontmatter, okf, raw, writer, Vault};
use saussunet_core::{boot_open, Store};

const V1: &str = "# Threshold Post\n\nRust is a systems language. It powers many tools.\n";

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
    format!("raw/{}-threshold-post.md", today())
}

fn pass1_central(name: &str, kind: &str) -> serde_json::Value {
    json!({
        "candidates": [
            {"name": name, "kind": kind, "mentions": 5, "central": true,
             "justification": format!("the article is about {name}"), "aliases": []}
        ]
    })
}

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

fn slug_map() -> HashMap<String, (String, String)> {
    HashMap::from([
        (
            "Foo".to_string(),
            ("Foo Title".to_string(), "/concepts/foo.md".to_string()),
        ),
        (
            "foo".to_string(),
            ("Foo Title".to_string(), "/concepts/foo.md".to_string()),
        ),
    ])
}

// ---------------------------------------------------------------------------
// Ported B3Wiki TestWikilinks ×5 (test_to_okf.py:13-51) → rewrite_wikilinks.
// ---------------------------------------------------------------------------

#[test]
fn wikilink_bare_uses_title() {
    // test_bare_link_uses_title: bare [[Foo]] → [Foo Title](/concepts/foo.md)
    let out = okf::rewrite_wikilinks("see [[Foo]]", &slug_map());
    assert_eq!(out, "see [Foo Title](/concepts/foo.md)");
}

#[test]
fn wikilink_aliased() {
    // test_aliased_link: alias wins over the map title.
    let out = okf::rewrite_wikilinks("the [[foo|the foo]] rocks", &slug_map());
    assert_eq!(out, "the [the foo](/concepts/foo.md) rocks");
}

#[test]
fn wikilink_path_style_resolves_via_stem() {
    // test_path_style_from_root: [[dir/path]] resolves via the last segment.
    let out = okf::rewrite_wikilinks("- [[concepts/foo]] - x", &slug_map());
    assert_eq!(out, "- [Foo Title](/concepts/foo.md) - x");
}

#[test]
fn wikilink_broken_tolerated() {
    // test_broken_link_tolerated (D-10): unresolvable → [slug](/slug.md).
    let out = okf::rewrite_wikilinks("see [[Nope]]", &slug_map());
    assert_eq!(out, "see [Nope](/Nope.md)");
}

#[test]
fn wikilink_traversal_sanitized() {
    // Threat model: a `../evil` slug must never yield a `..` path — it falls
    // through to the broken form with the traversal stripped.
    let out = okf::rewrite_wikilinks("x [[../evil]] y", &slug_map());
    assert!(!out.contains(".."), "no traversal in output: {out}");
    assert_eq!(out, "x [evil](/evil.md) y");
}

#[test]
fn wikilink_alias_cannot_escape_link_syntax() {
    // Threat model: alias text is LLM-authored; `]` is stripped so it cannot
    // break out of the [text](...) syntax.
    let out = okf::rewrite_wikilinks("[[foo|a]b]]", &slug_map());
    assert_eq!(out, "[ab](/concepts/foo.md)");
}

#[test]
fn wikilink_embeds_untouched() {
    // `![[embed]]` is an image, not a page link — kept verbatim.
    let out = okf::rewrite_wikilinks("pic ![[img.png]] see [[Foo]]", &slug_map());
    assert_eq!(out, "pic ![[img.png]] see [Foo Title](/concepts/foo.md)");
}

// ---------------------------------------------------------------------------
// count_outbound_markdown_links (okflint MD_LINK shape: `](/target)`).
// ---------------------------------------------------------------------------

#[test]
fn markdown_links_counted_deduped() {
    assert_eq!(
        frontmatter::count_outbound_markdown_links("see [a](/x.md) and [b](/y.md)"),
        2
    );
    // Deduped by target, like the wikilink counter.
    assert_eq!(
        frontmatter::count_outbound_markdown_links("[a](/x.md) [b](/x.md)"),
        1
    );
    // Absolute http(s) links are not bundle-relative.
    assert_eq!(
        frontmatter::count_outbound_markdown_links("[a](http://x.com) [b](/x.md)"),
        1
    );
    // Bare relative paths are not the D-11 root-relative form.
    assert_eq!(
        frontmatter::count_outbound_markdown_links("[a](rel.md)"),
        0
    );
    assert_eq!(frontmatter::count_outbound_markdown_links("no links"), 0);
}

// ---------------------------------------------------------------------------
// T2 — end-to-end: zero wikilinks on disk after a MockProvider ingest.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn no_wikilinks_on_disk() {
    let (_tmp, _id, store, vault) = setup().await;
    let url = "https://okf.example/links";
    serve(url, V1);
    let provider = Arc::new(MockProvider::new(vec![
        Scripted::Ok(pass1_central("React", "concept")),
        Scripted::Ok(page_json("React", "concept")),
    ]));
    run_ingest(&store, &vault, &provider_arc(&provider), url, url).await;

    let content =
        std::fs::read_to_string(vault.root().join("concepts/react.md")).expect("page on disk");
    let (_yaml, body) = frontmatter::split(&content);
    assert!(
        !content.contains("[["),
        "zero wikilinks may reach disk: {content}"
    );
    // The fixture's whitelist links resolve via the index.md stems source.
    assert!(
        body.contains("[Meta](/concepts/Meta.md)"),
        "index-stem link rewritten: {body}"
    );
    assert!(
        body.contains("[JavaScript](/concepts/JavaScript.md)"),
        "index-stem link rewritten: {body}"
    );
    assert!(
        frontmatter::count_outbound_markdown_links(body) >= 2,
        "floor survives on markdown links: {body}"
    );
}

#[tokio::test]
async fn markdown_floor_rejects_generated_page() {
    // A generated page whose post-rewrite markdown links < 2 is rejected
    // with the floor error naming markdown links (D-10).
    let (_tmp, _id, store, vault) = setup().await;
    let fm = raw::RawFrontmatter {
        title: "Evidence".into(),
        source_url: "https://e.example".into(),
        ingested: "2026-10-30T12:00:00+00:00".into(),
        sha256: "aa".into(),
    };
    raw::write_raw(&vault, None, "evidence", "md", &fm, b"e body")
        .await
        .unwrap();
    let src = format!("raw/{}-evidence.md", today());
    let content = format!(
        "---\ntitle: Lonely\ntype: concept\ncreated: {d}\nupdated: {d}\ntags: [technology]\nsources: [{s}]\n---\n\nLonely is sparse. ^[{s}] See [[Meta]].\n",
        d = today(),
        s = src
    );
    let err = writer::write_generated_page(&vault, &store, "concepts/lonely.md", &content)
        .await
        .expect_err("one outbound markdown link must not pass the floor");
    let msg = err.to_string();
    assert!(
        msg.contains("markdown link"),
        "floor error must name markdown links: {msg}"
    );
    assert!(
        !vault.root().join("concepts/lonely.md").exists(),
        "rejected page must not be written"
    );
}
