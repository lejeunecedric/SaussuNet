//! Plan 04.1-02 (D-17/D-18/D-19): index.md OKF rework + log.md date groups.
//!
//! T6 (`index_okf`) is the PAIRED Pitfall-1 pin: it asserts the writer side
//! (`index::seed`/`upsert_entry`) and the parser side (`validate::link_whitelist`
//! / `validate::index_entry_map`) agree on the new `- [Title](/path.md) — desc`
//! format in ONE test, so no commit boundary can silently empty the whitelist.
//! T7 (`log_date_groups`) pins the date-grouped, newest-first log shape.

use std::sync::Arc;

use tempfile::TempDir;

use saussunet_core::config::{FileConfig, RuntimeConfig};
use saussunet_core::vault::index;
use saussunet_core::vault::validate::{index_entry_map, link_whitelist};
use saussunet_core::vault::Vault;
use saussunet_core::{boot_open, Store};

async fn setup() -> (TempDir, Arc<Store>, Arc<Vault>) {
    let tmp = TempDir::new().expect("tempdir");
    let toml = format!(
        "[storage]\ndata_dir = \"{}\"\n\n[vault]\nid = 1\nname = \"main\"\n",
        tmp.path().display()
    );
    let cfg: FileConfig = toml::from_str(&toml).expect("config parses");
    let rt: RuntimeConfig = cfg.resolve().expect("resolve");
    let (store, vault) = boot_open(&rt).await.expect("boot");
    (tmp, Arc::new(store), Arc::new(vault))
}

fn today() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

// ---------------------------------------------------------------------------
// T6 — index OKF format, pinned from BOTH sides (Pitfall 1)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn index_okf() {
    // (a) D-17: seed() starts with the exact three-line frontmatter block.
    let seed = index::seed();
    assert!(
        seed.starts_with("---\nokf_version: \"0.2\"\n---\n"),
        "seed must start with the okf_version frontmatter block, got:\n{}",
        &seed[..seed.len().min(80)]
    );

    let (_tmp, store, vault) = setup().await;

    // (b) D-18: new entry shape + counter counts it.
    index::upsert_entry(&vault, "Concepts", "foo", "Foo Title", Some("a desc"))
        .await
        .expect("upsert foo");
    let index_md = std::fs::read_to_string(vault.root().join("index.md")).unwrap();
    assert!(
        index_md.contains("- [Foo Title](/concepts/foo.md) — a desc"),
        "new entry format missing:\n{index_md}"
    );
    assert!(
        index_md.contains(&format!(
            "> Last updated: {} | Total pages: 1",
            today()
        )),
        "counter must count the new-format entry:\n{index_md}"
    );

    // Entry without description: no trailing em-dash.
    index::upsert_entry(&vault, "Concepts", "bar", "Bar Title", None)
        .await
        .expect("upsert bar");
    let index_md = std::fs::read_to_string(vault.root().join("index.md")).unwrap();
    assert!(
        index_md.contains("- [Bar Title](/concepts/bar.md)\n"),
        "no-description entry must have no trailing em-dash:\n{index_md}"
    );
    assert!(index_md.contains("Total pages: 2"));

    // Re-upsert same path replaces (dedup keyed by path), counter stays.
    index::upsert_entry(&vault, "Concepts", "foo", "Foo Title v2", Some("new desc"))
        .await
        .expect("re-upsert foo");
    let index_md = std::fs::read_to_string(vault.root().join("index.md")).unwrap();
    assert!(index_md.contains("- [Foo Title v2](/concepts/foo.md) — new desc"));
    assert!(!index_md.contains("Foo Title]"));
    assert!(index_md.contains("Total pages: 2"), "dedup by path:\n{index_md}");

    // (c) parser side: whitelist non-empty + stem recovered from the path.
    let wl = link_whitelist(&vault, &store).await.expect("whitelist");
    assert!(!wl.is_empty(), "Pitfall-1 pin: whitelist must not be empty");
    assert!(wl.contains("foo"), "stem-from-path: {wl:?}");
    assert!(wl.contains("bar"), "stem-from-path: {wl:?}");
    let map = index_entry_map(&index_md);
    assert_eq!(
        map.get("foo").map(String::as_str),
        Some("/concepts/foo.md"),
        "stem→path map: {map:?}"
    );

    // (d) path traversal: a forged `..` entry is never whitelisted.
    let mut forged = std::fs::read_to_string(vault.root().join("index.md")).unwrap();
    forged.push_str("- [Evil](/../etc/passwd.md) — traversal\n");
    std::fs::write(vault.root().join("index.md"), forged).unwrap();
    let wl = link_whitelist(&vault, &store).await.expect("whitelist");
    assert!(!wl.contains("passwd"), "traversal path must be skipped: {wl:?}");
    assert!(!wl.contains("etc"), "traversal path must be skipped: {wl:?}");
}

