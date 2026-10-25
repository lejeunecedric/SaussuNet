//! Task-1 coverage for Plan 01-04: `reindex` (D-01 rebuildability proof),
//! the minimal vault-id-keyed store APIs for `jobs` / `sync_state` /
//! `entity_registry` (D-2 minimal boundary), and vault-id isolation of the
//! registry rebuild (threat T-04-03).

use std::path::Path;

use tempfile::TempDir;

use saussunet_core::config::{FileConfig, RuntimeConfig};
use saussunet_core::reindex::reindex;
use saussunet_core::vault::writer::write_page;
use saussunet_core::vault::Vault;
use saussunet_core::{boot_open, Store};

fn rt_for(data_dir: &Path, name: &str) -> RuntimeConfig {
    let toml = format!(
        "[storage]\ndata_dir = \"{}\"\n\n[vault]\nid = 1\nname = \"{name}\"\n",
        data_dir.display()
    );
    let cfg: FileConfig = toml::from_str(&toml).expect("test config parses");
    cfg.resolve()
}

async fn setup() -> (TempDir, Vault, Store) {
    let tmp = TempDir::new().expect("tempdir");
    let rt = rt_for(tmp.path(), "main");
    let (store, vault) = boot_open(&rt).await.expect("boot");
    (tmp, vault, store)
}

fn page(title: &str) -> String {
    format!(
        "---\ntitle: {title}\ncreated: 2020-01-01\nupdated: 2020-01-01\ntype: concept\ntags: [technology]\nsources: [raw/articles/src.md]\n---\n\nIntro for {title}. See [[link-a]] and [[link-b]].\n"
    )
}

#[tokio::test]
async fn reindex_rebuilds_registry_after_wipe() {
    let (_tmp, vault, store) = setup().await;

    // 3 pages through the writer (each carries a gen_hash map).
    for (rel, title) in [
        ("concepts/alpha.md", "Alpha"),
        ("concepts/beta.md", "Beta"),
        ("entities/gamma.md", "Gamma"),
    ] {
        write_page(&vault, &store, rel, &page(title))
            .await
            .expect("writer write");
    }
    // 1 hand-made page: valid frontmatter, NO gen_hash map (human-owned).
    std::fs::write(
        vault.root().join("concepts/hand.md"),
        "---\ntitle: Hand Made\ncreated: 2020-01-01\nupdated: 2020-01-02\ntype: concept\ntags: [technology]\nsources: [raw/articles/h.md]\n---\n\nHandwritten. See [[alpha]].\n",
    )
    .expect("hand page");
    // 1 malformed-frontmatter file: invalid YAML flow sequence.
    std::fs::write(
        vault.root().join("concepts/broken.md"),
        "---\ntitle: [unclosed\n---\nbody\n",
    )
    .expect("broken page");
    // Dot-prefixed file must be invisible to the scan (writer temp convention).
    std::fs::write(
        vault.root().join("concepts/.alpha.md.tmp-deadbeef"),
        "torn temp",
    )
    .expect("temp file");

    // Wipe the projection (the destructive half of the D-01 proof).
    store.clear_pages(vault.id).await.expect("clear");
    assert!(store.list_pages(vault.id).await.unwrap().is_empty());

    // The malformed file prints `skipped ...` to stderr and is excluded.
    let n = reindex(&vault, &store).await.expect("reindex");
    assert_eq!(n, 4, "3 writer pages + 1 hand-made page");

    let rows = store.list_pages(vault.id).await.unwrap();
    assert_eq!(rows.len(), 4);
    let by_path: std::collections::BTreeMap<_, _> =
        rows.iter().map(|r| (r.path.as_str(), r)).collect();
    assert!(!by_path.contains_key("concepts/broken.md"));
    assert!(!by_path.contains_key("concepts/.alpha.md.tmp-deadbeef"));
    for rel in ["concepts/alpha.md", "concepts/beta.md", "entities/gamma.md"] {
        assert!(by_path[rel].has_gen_hash, "{rel} written by writer");
    }
    let hand = by_path["concepts/hand.md"];
    assert!(!hand.has_gen_hash, "hand-made page has no gen_hash map");
    assert_eq!(hand.title, "Hand Made");
    assert_eq!(hand.page_type, "concept");
    assert_eq!(hand.updated.as_deref(), Some("2020-01-02"));

    // Idempotent: a second reindex lands on the same 4 rows.
    assert_eq!(reindex(&vault, &store).await.unwrap(), 4);
    assert_eq!(store.list_pages(vault.id).await.unwrap().len(), 4);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_write_page_and_reindex_keep_registry_consistent() {
    // Review W-6: `reindex` must hold the vault write lock for its whole
    // scan+rebuild and do clear+repopulate in ONE transaction. Proof:
    // while reindex hammers, a concurrent reader must NEVER observe a
    // partial/empty registry (pre-fix the committed clear was visible
    // between the clear and the upserts), and a concurrent write_page's
    // row must never be lost (pre-fix its upsert could land between the
    // reindex scan and the clear).
    use std::sync::atomic::{AtomicBool, Ordering};
    let (_tmp, vault, store) = setup().await;
    let vault = std::sync::Arc::new(vault);
    let store = std::sync::Arc::new(store);
    for i in 0..6 {
        let rel = format!("concepts/c{i}.md");
        write_page(&vault, &store, &rel, &page(&format!("C{i}")))
            .await
            .expect("seed write");
    }

    let stop = std::sync::Arc::new(AtomicBool::new(false));

    // Reindex hammer.
    let (v, s, st) = (vault.clone(), store.clone(), stop.clone());
    let hammer = tokio::spawn(async move {
        for _ in 0..200 {
            reindex(&v, &s).await.expect("reindex");
        }
        st.store(true, Ordering::SeqCst);
    });
    // Concurrent writer mid-hammer.
    let (v, s) = (vault.clone(), store.clone());
    let writer =
        tokio::spawn(async move { write_page(&v, &s, "concepts/late.md", &page("Late")).await });
    // Reader: the registry must always hold all 6 seeded pages.
    let (v, s, st) = (vault.clone(), store.clone(), stop.clone());
    let reader = tokio::spawn(async move {
        let mut partial = 0usize;
        while !st.load(Ordering::SeqCst) {
            let rows = s.list_pages(v.id).await.unwrap();
            if rows.len() < 6 {
                partial += 1;
            }
        }
        partial
    });

    hammer.await.expect("hammer");
    writer.await.expect("writer").expect("late write");
    let partial = reader.await.expect("reader");
    assert_eq!(
        partial, 0,
        "reader saw a torn (partial) registry {partial} times"
    );

    // Settled state: every page on disk has its row, nothing lost.
    let rows = store.list_pages(vault.id).await.unwrap();
    for i in 0..6 {
        assert!(
            rows.iter().any(|r| r.path == format!("concepts/c{i}.md")),
            "registry lost concepts/c{i}.md under concurrent reindex"
        );
    }
    assert!(rows.iter().any(|r| r.path == "concepts/late.md"));
    assert_eq!(reindex(&vault, &store).await.unwrap(), 7);
    assert_eq!(store.list_pages(vault.id).await.unwrap().len(), 7);
}

#[tokio::test]
async fn reindex_leaves_other_vault_rows_intact() {
    // T-04-03: clear/repopulate is scoped WHERE vault_id = ? — a second
    // vault's registry must survive a reindex of the first.
    let (_tmp, vault, store) = setup().await;
    store.ensure_vault(2, "second").await.expect("vault 2");
    store
        .upsert_page(
            2,
            "concepts/other.md",
            "Other",
            "concept",
            "[]",
            None,
            false,
        )
        .await
        .expect("vault 2 row");
    write_page(&vault, &store, "concepts/own.md", &page("Own"))
        .await
        .expect("writer write");

    assert_eq!(reindex(&vault, &store).await.unwrap(), 1);
    assert_eq!(store.list_pages(vault.id).await.unwrap().len(), 1);
    let other = store.list_pages(2).await.unwrap();
    assert_eq!(other.len(), 1, "vault 2 rows untouched");
    assert_eq!(other[0].path, "concepts/other.md");
}

#[tokio::test]
async fn reindex_skips_symlink_escape() {
    // T-04-01: a symlink planted inside the vault must not pull external
    // files into the registry.
    let (_tmp, vault, store) = setup().await;
    let outside = _tmp.path().join("outside-secret.md");
    std::fs::write(
        &outside,
        "---\ntitle: Secret\ntype: concept\n---\nsecret body\n",
    )
    .expect("outside file");
    std::os::unix::fs::symlink(
        outside.canonicalize().unwrap(),
        vault.root().join("concepts/link.md"),
    )
    .expect("symlink");

    let n = reindex(&vault, &store).await.expect("reindex");
    assert_eq!(n, 0, "symlinked page must not be indexed");
    assert!(store.list_pages(vault.id).await.unwrap().is_empty());
}

#[tokio::test]
async fn jobs_round_trip_with_vault_isolation() {
    let (_tmp, vault, store) = setup().await;
    store.ensure_vault(2, "second").await.expect("vault 2");

    let id1 = store
        .insert_job(vault.id, "ingest", "{}")
        .await
        .expect("job 1");
    let id2 = store
        .insert_job(vault.id, "lint", "{}")
        .await
        .expect("job 2");
    assert!(id1 > 0 && id2 > 0);
    store.insert_job(2, "ingest", "{}").await.expect("job v2");

    let mine = store.list_jobs(vault.id, None).await.unwrap();
    assert_eq!(mine.len(), 2);
    assert!(mine.iter().all(|j| j.vault_id == vault.id));
    assert!(mine.iter().all(|j| j.status == "pending"));

    let theirs = store.list_jobs(2, None).await.unwrap();
    assert_eq!(theirs.len(), 1, "vault 2 sees only its own job");

    // Status filter (claim-order surface for Phase 2; no claim logic here).
    assert_eq!(
        store
            .list_jobs(vault.id, Some("pending"))
            .await
            .unwrap()
            .len(),
        2
    );
    assert!(store
        .list_jobs(vault.id, Some("done"))
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn sync_state_upsert_updates_token_and_etag() {
    let (_tmp, vault, store) = setup().await;

    store
        .upsert_sync_state(
            vault.id,
            "webdav:main",
            Some("t1"),
            Some("e1"),
            Some("2026-10-25T00:00:00Z"),
            None,
        )
        .await
        .expect("first upsert");
    let s = store
        .get_sync_state(vault.id, "webdav:main")
        .await
        .unwrap()
        .expect("present");
    assert_eq!(s.token.as_deref(), Some("t1"));
    assert_eq!(s.etag.as_deref(), Some("e1"));

    // Second upsert on the same (vault_id, source_id) key UPDATES.
    store
        .upsert_sync_state(
            vault.id,
            "webdav:main",
            Some("t2"),
            Some("e2"),
            Some("2026-10-26T00:00:00Z"),
            Some("boom"),
        )
        .await
        .expect("second upsert");
    let s = store
        .get_sync_state(vault.id, "webdav:main")
        .await
        .unwrap()
        .expect("present");
    assert_eq!(s.token.as_deref(), Some("t2"));
    assert_eq!(s.etag.as_deref(), Some("e2"));
    assert_eq!(s.last_error.as_deref(), Some("boom"));

    // Vault-scoped: another vault cannot see this row (VLET-04).
    store.ensure_vault(2, "second").await.expect("vault 2");
    assert!(store
        .get_sync_state(2, "webdav:main")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn resolve_entity_matches_canonical_and_alias() {
    let (_tmp, vault, store) = setup().await;
    store
        .upsert_entity(
            vault.id,
            "Marie Curie",
            r#"["Curie, Marie", "Sklodowska"]"#,
            Some("entities/marie-curie.md"),
            Some("entity"),
        )
        .await
        .expect("entity");

    // Canonical name, case-insensitive + whitespace-collapsed.
    let hit = store
        .resolve_entity(vault.id, "  MARIE   CURIE ")
        .await
        .unwrap()
        .expect("canonical match");
    assert_eq!(hit.canonical_name, "Marie Curie");

    // Alias, case-insensitive.
    let hit = store
        .resolve_entity(vault.id, "curie, marie")
        .await
        .unwrap()
        .expect("alias match");
    assert_eq!(hit.canonical_name, "Marie Curie");

    // Unknown names miss; other vaults miss (VLET-04).
    assert!(store
        .resolve_entity(vault.id, "Einstein")
        .await
        .unwrap()
        .is_none());
    assert!(store
        .resolve_entity(vault.id, "  ")
        .await
        .unwrap()
        .is_none());
    store.ensure_vault(2, "second").await.expect("vault 2");
    assert!(store
        .resolve_entity(2, "marie curie")
        .await
        .unwrap()
        .is_none());
}
