//! Deterministic write-path validator (plan 03-06, PIPE-06 / D-11): every
//! generated page must pass link-whitelist + frontmatter + provenance checks
//! BEFORE any disk write. Rejection matrix + acceptance cases; every rejection
//! message must name the offending link/marker/source.

use std::sync::Arc;

use tempfile::TempDir;

use saussunet_core::config::{FileConfig, RuntimeConfig};
use saussunet_core::error::CoreError;
use saussunet_core::vault::index;
use saussunet_core::vault::validate::{
    extract_provenance, extract_wikilinks, link_whitelist, validate_generated_page,
};
use saussunet_core::vault::Vault;
use saussunet_core::{boot_open, Store};

async fn setup() -> (TempDir, i64, Arc<Store>, Arc<Vault>) {
    let tmp = TempDir::new().expect("tempdir");
    let toml = format!(
        "[storage]\ndata_dir = \"{}\"\n\n[vault]\nid = 1\nname = \"main\"\n",
        tmp.path().display()
    );
    let cfg: FileConfig = toml::from_str(&toml).expect("config parses");
    let rt: RuntimeConfig = cfg.resolve().expect("resolve");
    let (store, vault) = boot_open(&rt).await.expect("boot");
    // The evidence layer: one real raw/ file every valid fixture cites.
    std::fs::create_dir_all(vault.root().join("raw")).expect("raw dir");
    std::fs::write(vault.root().join("raw/src.md"), "# source\n").expect("raw file");
    // Two pre-existing wiki pages, discoverable via index.md stems.
    index::upsert_entry(&vault, "Concepts", "Meta", "Meta")
        .await
        .expect("index Meta");
    index::upsert_entry(&vault, "Concepts", "JavaScript", "JavaScript")
        .await
        .expect("index JavaScript");
    (tmp, vault.id, Arc::new(store), Arc::new(vault))
}

/// Generated-page fixture with configurable sources line and body.
fn gen_page(sources: &str, body: &str) -> String {
    format!(
        "---\ntitle: Test Page\ncreated: 2020-01-01\nupdated: 2020-01-01\n\
         type: concept\ntags: [technology]\nsources: [{sources}]\n---\n\n{body}\n"
    )
}

/// A fully valid generated page: schema frontmatter, raw/ source that
/// exists, provenance marker to a real raw/ file, whitelisted links only.
fn valid_body() -> String {
    "Test page is about things. ^[raw/src.md] See [[Meta]] and [[JavaScript]].".to_string()
}

fn validation_msg(err: CoreError) -> String {
    match err {
        CoreError::Validation(msg) => msg,
        other => panic!("expected CoreError::Validation, got {other:?}"),
    }
}

#[tokio::test]
async fn valid_generated_page_accepted() {
    let (_tmp, _id, store, vault) = setup().await;
    validate_generated_page(
        &vault,
        &store,
        "concepts/test-page.md",
        &gen_page("raw/src.md", &valid_body()),
    )
    .await
    .expect("valid page must pass");
}

#[tokio::test]
async fn dangling_wikilink_rejected_and_named() {
    let (_tmp, _id, store, vault) = setup().await;
    let body = "Test page. ^[raw/src.md] See [[Meta]] and [[Nonexistent Page]].";
    let err = validate_generated_page(
        &vault,
        &store,
        "concepts/test-page.md",
        &gen_page("raw/src.md", body),
    )
    .await
    .expect_err("dangling link must be rejected");
    let msg = validation_msg(err);
    assert!(
        msg.contains("Nonexistent Page"),
        "message names target: {msg}"
    );
}

#[tokio::test]
async fn missing_provenance_marker_rejected() {
    let (_tmp, _id, store, vault) = setup().await;
    let body = "Test page with no provenance at all. See [[Meta]] and [[JavaScript]].";
    let err = validate_generated_page(
        &vault,
        &store,
        "concepts/test-page.md",
        &gen_page("raw/src.md", body),
    )
    .await
    .expect_err("zero markers must be rejected");
    let msg = validation_msg(err);
    assert!(msg.contains("provenance"), "message names rule: {msg}");
}

#[tokio::test]
async fn provenance_marker_to_nonexistent_raw_rejected() {
    let (_tmp, _id, store, vault) = setup().await;
    let body = "Test page. ^[raw/does-not-exist.md] See [[Meta]] and [[JavaScript]].";
    let err = validate_generated_page(
        &vault,
        &store,
        "concepts/test-page.md",
        &gen_page("raw/src.md", body),
    )
    .await
    .expect_err("nonexistent marker target must be rejected");
    let msg = validation_msg(err);
    assert!(
        msg.contains("raw/does-not-exist.md"),
        "message names marker: {msg}"
    );
}

#[tokio::test]
async fn wiki_layer_source_rejected_pitfall4() {
    let (_tmp, _id, store, vault) = setup().await;
    // Belt-and-braces: a wiki page cited as the source (self-citation loop).
    let page = gen_page("entities/foo.md", &valid_body());
    let err = validate_generated_page(&vault, &store, "concepts/test-page.md", &page)
        .await
        .expect_err("wiki-as-source must be rejected");
    let msg = validation_msg(err);
    assert!(
        msg.contains("entities/foo.md"),
        "message names source: {msg}"
    );
}

#[tokio::test]
async fn nonexistent_raw_source_rejected() {
    let (_tmp, _id, store, vault) = setup().await;
    let page = gen_page("raw/missing.md", &valid_body());
    let err = validate_generated_page(&vault, &store, "concepts/test-page.md", &page)
        .await
        .expect_err("nonexistent source file must be rejected");
    let msg = validation_msg(err);
    assert!(
        msg.contains("raw/missing.md"),
        "message names source: {msg}"
    );
}

#[tokio::test]
async fn empty_sources_rejected() {
    let (_tmp, _id, store, vault) = setup().await;
    let page = gen_page("", &valid_body());
    let err = validate_generated_page(&vault, &store, "concepts/test-page.md", &page)
        .await
        .expect_err("empty sources must be rejected");
    let msg = validation_msg(err);
    assert!(msg.contains("sources"), "message names rule: {msg}");
}

#[tokio::test]
async fn registry_entity_not_in_index_accepted() {
    let (_tmp, vault_id, store, vault) = setup().await;
    // Registry-only entity: no index.md entry yet — whitelist includes registry.
    store
        .upsert_entity(
            vault_id,
            "Gamma",
            "[]",
            Some("entities/gamma.md"),
            Some("entity"),
        )
        .await
        .unwrap();
    let body = "Test page. ^[raw/src.md] See [[Gamma]] and [[Meta]].";
    validate_generated_page(
        &vault,
        &store,
        "concepts/test-page.md",
        &gen_page("raw/src.md", body),
    )
    .await
    .expect("registry entity must be whitelisted");
}

#[tokio::test]
async fn own_stem_link_accepted() {
    let (_tmp, _id, store, vault) = setup().await;
    let body = "Test page. ^[raw/src.md] See [[Meta]] and [[test-page]].";
    validate_generated_page(
        &vault,
        &store,
        "concepts/test-page.md",
        &gen_page("raw/src.md", body),
    )
    .await
    .expect("a page may link its own stem");
}

#[tokio::test]
async fn provenance_marker_outside_raw_rejected() {
    let (_tmp, _id, store, vault) = setup().await;
    let body = "Test page. ^[entities/other.md] See [[Meta]] and [[JavaScript]].";
    let err = validate_generated_page(
        &vault,
        &store,
        "concepts/test-page.md",
        &gen_page("raw/src.md", body),
    )
    .await
    .expect_err("marker outside raw/ must be rejected");
    let msg = validation_msg(err);
    assert!(
        msg.contains("entities/other.md"),
        "message names marker: {msg}"
    );
}

#[tokio::test]
async fn helpers_extract_targets_correctly() {
    // Aliases and heading anchors strip to the target; embeds are skipped.
    let links =
        extract_wikilinks("See [[Alpha|the alpha]] then [[Beta#heading]] and ![[img.png]].");
    assert_eq!(links, vec!["Alpha".to_string(), "Beta".to_string()]);
    // Provenance markers: every `^[...]` target, deduped in order.
    let prov = extract_provenance("a ^[raw/x.md] b ^[raw/y.md] c ^[raw/x.md]");
    assert_eq!(prov, vec!["raw/x.md".to_string(), "raw/y.md".to_string()]);
}

#[tokio::test]
async fn link_whitelist_covers_index_and_registry() {
    let (_tmp, vault_id, store, vault) = setup().await;
    store
        .upsert_entity(
            vault_id,
            "Gamma",
            "[]",
            Some("entities/gamma.md"),
            Some("entity"),
        )
        .await
        .unwrap();
    let wl = link_whitelist(&vault, &store).await.expect("whitelist");
    assert!(wl.contains("Meta"), "index stem");
    assert!(wl.contains("JavaScript"), "index stem");
    assert!(wl.contains("Gamma"), "registry canonical name");
    assert!(wl.contains("gamma"), "registry page_path stem");
}
