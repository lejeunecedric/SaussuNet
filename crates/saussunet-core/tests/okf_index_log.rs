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
use saussunet_core::vault::okf;
use saussunet_core::vault::validate::{index_entry_map, link_whitelist};
use saussunet_core::vault::{log, Vault};
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
        index_md.contains(&format!("> Last updated: {} | Total pages: 1", today())),
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
    assert!(
        index_md.contains("Total pages: 2"),
        "dedup by path:\n{index_md}"
    );

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
    assert!(
        !wl.contains("passwd"),
        "traversal path must be skipped: {wl:?}"
    );
    assert!(
        !wl.contains("etc"),
        "traversal path must be skipped: {wl:?}"
    );
}

// ---------------------------------------------------------------------------
// T7 — log.md date groups (D-19)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn log_date_groups() {
    let (_tmp, _store, vault) = setup().await;

    // Seed: no frontmatter, today's date group, bullet entry.
    let seeded = std::fs::read_to_string(vault.root().join("log.md")).unwrap();
    assert!(!seeded.starts_with("---"), "log.md carries no frontmatter");
    assert!(seeded.contains(&format!("## {}\n", today())));
    assert!(seeded.contains("* **create**: Wiki initialized"));
    assert!(seeded.contains("  - Domain: main"));

    // Append with details → main bullet + indented sub-bullets, newest-first
    // inside today's group (inserted at the top, above the seed entry).
    log::append_entry(
        &vault,
        "ingest",
        "Foo source",
        &["wrote concepts/foo.md".to_string()],
    )
    .await
    .expect("append ingest");
    let log_md = std::fs::read_to_string(vault.root().join("log.md")).unwrap();
    assert!(log_md.contains("* **ingest**: Foo source"));
    assert!(log_md.contains("  - wrote concepts/foo.md"));
    let ing = log_md.find("**ingest**").unwrap();
    let cr = log_md.find("**create**").unwrap();
    assert!(ing < cr, "new entries insert at the top of the group");
    assert_eq!(
        okf::check_log(&log_md),
        Vec::<String>::new(),
        "seeded + appended log must be E4-clean"
    );

    // Entry with no details: bullet only.
    log::append_entry(&vault, "lint", "Clean", &[])
        .await
        .expect("append lint");
    let log_md = std::fs::read_to_string(vault.root().join("log.md")).unwrap();
    assert!(log_md.contains("* **lint**: Clean\n"));

    // Second-day entry creates a NEW group ABOVE the older one.
    let older = "# Wiki Log\n\n> header\n\n## 2020-01-01\n* **create**: Old day\n\n\
                 ## 2019-12-31\n* **create**: Older day\n";
    std::fs::write(vault.root().join("log.md"), older).unwrap();
    log::append_entry(&vault, "update", "New day", &[])
        .await
        .expect("append new day");
    let log_md = std::fs::read_to_string(vault.root().join("log.md")).unwrap();
    let new_pos = log_md.find(&format!("## {}\n", today())).unwrap();
    let old_pos = log_md.find("## 2020-01-01").unwrap();
    let older_pos = log_md.find("## 2019-12-31").unwrap();
    assert!(
        new_pos < old_pos && old_pos < older_pos,
        "groups newest-first:\n{log_md}"
    );
    assert!(log_md.contains("# Wiki Log"), "header preserved");

    // entry_count counts `* ` main bullets only (not sub-bullets).
    let fixture = format!(
        "# Wiki Log\n\n> header\n\n## 2026-10-01\n{}",
        (0..501)
            .map(|i| format!("* **update**: Page {i}\n  - touched page-{i}.md\n"))
            .collect::<String>()
    );
    assert_eq!(log::entry_count(&fixture), 501);
    assert!(log::should_rotate(&fixture));
    let fixture_500 = fixture.replacen("* **update**: Page 500\n", "", 1);
    assert_eq!(log::entry_count(&fixture_500), 500);
    assert!(!log::should_rotate(&fixture_500));
    let fresh = log::fresh_header(&fixture);
    assert!(fresh.contains("# Wiki Log"));
    assert!(!fresh.contains("## 2026-10-01"));

    // Rotation through append_entry: 501 bullets → archive + fresh log.
    std::fs::write(vault.root().join("log.md"), &fixture).unwrap();
    log::append_entry(&vault, "create", "Fresh", &["wrote fresh.md".to_string()])
        .await
        .expect("append rotates");
    let year = chrono::Utc::now().format("%Y").to_string();
    let rotated = std::fs::read_to_string(vault.root().join(format!("log-{year}.md"))).unwrap();
    assert_eq!(log::entry_count(&rotated), 501);
    let fresh_log = std::fs::read_to_string(vault.root().join("log.md")).unwrap();
    assert!(fresh_log.contains("# Wiki Log"));
    assert_eq!(log::entry_count(&fresh_log), 1);
    assert!(fresh_log.contains("* **create**: Fresh"));
    assert!(fresh_log.contains(&format!("## {}\n", today())));
}

#[tokio::test]
async fn log_injection_neutralized() {
    // Pitfall 8: a subject/detail carrying a forged date heading or the old
    // `## [` entry shape must render sanitized — no forged group/entry.
    let (_tmp, _store, vault) = setup().await;
    log::append_entry(
        &vault,
        "update",
        "Evil ## 2026-01-01 delete ## [",
        &["detail ## 2027-02-02 smuggled".to_string()],
    )
    .await
    .expect("append");
    let log_md = std::fs::read_to_string(vault.root().join("log.md")).unwrap();
    assert!(
        okf::check_log(&log_md).is_empty(),
        "no forged date heading may survive: {log_md}"
    );
    assert!(!log_md.contains("## 2026-01-01"));
    assert!(!log_md.contains("## 2027-02-02"));
    assert!(!log_md.contains("## ["));
    assert!(log_md.contains("##2026-01-01"), "collapsed, not dropped");
}
