//! End-to-end walking-skeleton tests (ROADMAP SC1/SC4-schema/SC5):
//! boot → WAL SQLite + full migrations → vault FS init, idempotent,
//! vault-id-keyed (D-02/D-03/D-04, INFR-02, VLET-04).

use std::path::{Path, PathBuf};

use saussunet_core::config::{FileConfig, RuntimeConfig};
use saussunet_core::{assert_outside_vault, boot, Store};
use tempfile::TempDir;

/// Build a RuntimeConfig pointing data_dir at `data_dir` (same resolution
/// path production uses: TOML → FileConfig → resolve).
fn rt_for(data_dir: &Path, name: &str) -> RuntimeConfig {
    let toml = format!(
        "[storage]\ndata_dir = \"{}\"\n\n[vault]\nid = 1\nname = \"{name}\"\n",
        data_dir.display()
    );
    let cfg: FileConfig = toml::from_str(&toml).expect("test config parses");
    cfg.resolve().expect("resolve")
}

async fn vault_row_count(store: &Store) -> i64 {
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM vaults")
        .fetch_one(store.pool())
        .await
        .expect("count vaults");
    n
}

#[tokio::test]
async fn boot_is_idempotent_and_rename_safe() {
    let tmp = TempDir::new().expect("tempdir");
    let rt = rt_for(tmp.path(), "main");

    // First boot creates everything.
    let root1 = boot(&rt).await.expect("first boot");
    assert!(root1.is_dir());

    // Sentinel proves a second boot never clobbers existing seeds.
    let schema = root1.join("SCHEMA.md");
    let before = std::fs::read_to_string(&schema).unwrap();
    std::fs::write(&schema, format!("{before}\nSENTINEL\n")).unwrap();

    let root2 = boot(&rt).await.expect("second boot");
    assert_eq!(root1, root2, "vault root must be stable across boots");
    let after = std::fs::read_to_string(&schema).unwrap();
    assert!(after.contains("SENTINEL"), "sentinel must survive re-boot");
    assert!(after.starts_with(&before), "seed content must be untouched");

    // Rename the vault in config (same id): still exactly one row, id
    // unchanged (ON CONFLICT(id) updates only the name), no AUTOINCREMENT
    // drift, and the directory stays data/vaults/<row id> (D-04).
    let rt2 = rt_for(tmp.path(), "renamed");
    let root3 = boot(&rt2).await.expect("boot after rename");
    assert_eq!(root2, root3, "renaming must not move the vault directory");

    let store = Store::connect(&rt.db_path).await.unwrap();
    assert_eq!(vault_row_count(&store).await, 1, "exactly one vault row");
    let (id, name): (i64, String) = sqlx::query_as("SELECT id, name FROM vaults")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(id, 1, "row id pinned to configured id (no drift)");
    assert_eq!(name, "renamed", "rename updates the name only");
    assert!(root3.ends_with(Path::new("vaults/1")));
}

#[tokio::test]
async fn file_db_opens_in_wal_mode() {
    let tmp = TempDir::new().expect("tempdir");
    let rt = rt_for(tmp.path(), "main");
    boot(&rt).await.expect("boot");
    let store = Store::connect(&rt.db_path).await.unwrap();
    let (mode,): (String,) = sqlx::query_as("PRAGMA journal_mode")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(mode.to_lowercase(), "wal", "INFR-02: WAL journal mode");
}

#[tokio::test]
async fn memory_db_has_full_schema_and_foreign_keys() {
    let store = Store::connect_memory().await.expect("memory store");

    // All six tables exist after migrate (D-02) — proves the ../../migrations
    // embed path is correct (PATTERNS §4.5).
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
    )
    .fetch_all(store.pool())
    .await
    .unwrap();
    let names: Vec<&str> = rows.iter().map(|(n,)| n.as_str()).collect();
    for table in [
        "vaults",
        "page_registry",
        "entity_registry",
        "jobs",
        "sync_state",
        "pending_proposals",
    ] {
        assert!(
            names.contains(&table),
            "missing table {table}, got {names:?}"
        );
    }

    // foreign_keys(true) is per-connection and must be ON (PATTERNS §4.2).
    let (fk,): (i64,) = sqlx::query_as("PRAGMA foreign_keys")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(fk, 1, "foreign keys must be enforced");

    // Every table except vaults carries a vault_id column (VLET-04, D-03).
    for table in [
        "page_registry",
        "entity_registry",
        "jobs",
        "sync_state",
        "pending_proposals",
    ] {
        // PRAGMA table_info columns: cid, name, type, notnull, dflt_value, pk.
        let cols: Vec<(i64, String)> = sqlx::query_as(&format!("PRAGMA table_info({table})"))
            .fetch_all(store.pool())
            .await
            .unwrap();
        let has_vault_id = cols.iter().any(|(_, name)| name == "vault_id");
        assert!(has_vault_id, "table {table} must carry vault_id (VLET-04)");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn memory_store_is_one_shared_database_under_concurrency() {
    // Review W-5: pool pinned to one connection (sqlx's guidance for
    // `sqlite::memory:` pools). Proof: under real concurrency every task
    // must see the SAME database — a regression to per-connection private
    // :memory: DBs would show "no such table" or empty reads here.
    let store = std::sync::Arc::new(Store::connect_memory().await.expect("memory store"));
    store
        .ensure_vault(1, "main")
        .await
        .expect("vault row (FK target for page_registry)");
    store
        .upsert_page(1, "concepts/seed.md", "Seed", "concept", "[]", None, false)
        .await
        .expect("seed row");

    // Contention probe: hold the pool's connection while a query runs.
    // With a pinned single connection the query waits for the holder and
    // sees the data; with a >1 pool over a private-cache :memory: URL a
    // second connection would be an empty DB and fail.
    let holder = store.pool().acquire().await.expect("acquire holder");
    let s2 = std::sync::Arc::clone(&store);
    let probe = tokio::spawn(async move { s2.list_pages(1).await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    drop(holder);
    let rows = probe
        .await
        .expect("task")
        .expect("list_pages must run on the shared connection, not a private empty DB");
    assert_eq!(rows.len(), 1, "same connection sees the data");

    // Concurrent writers all land in the one shared database.
    let mut handles = Vec::new();
    for i in 0..8 {
        let store = std::sync::Arc::clone(&store);
        handles.push(tokio::spawn(async move {
            store
                .upsert_page(
                    1,
                    &format!("concepts/p{i}.md"),
                    &format!("P{i}"),
                    "concept",
                    "[]",
                    None,
                    false,
                )
                .await
                .expect("upsert (second private DB would lack the table)");
            let rows = store.list_pages(1).await.expect("list");
            assert!(
                rows.iter().any(|r| r.path == format!("concepts/p{i}.md")),
                "task {i} must see its own row in the shared DB"
            );
        }));
    }
    for h in handles {
        h.await.expect("task");
    }
    // Final state: seed + 8 rows live in the ONE shared database.
    assert_eq!(store.list_pages(1).await.unwrap().len(), 9);
}

#[tokio::test]
async fn init_fs_creates_expected_tree_and_seeds() {
    let tmp = TempDir::new().expect("tempdir");
    let rt = rt_for(tmp.path(), "main");
    let root = boot(&rt).await.expect("boot");

    for dir in [
        "raw/articles",
        "raw/papers",
        "raw/transcripts",
        "raw/assets",
        "entities",
        "concepts",
        "comparisons",
        "queries",
    ] {
        assert!(root.join(dir).is_dir(), "missing dir {dir}");
    }

    let schema = std::fs::read_to_string(root.join("SCHEMA.md")).unwrap();
    assert!(schema.contains("## Tag Taxonomy"));
    for tag in ["person", "technology", "process"] {
        assert!(schema.contains(tag));
    }
    let index = std::fs::read_to_string(root.join("index.md")).unwrap();
    assert!(index.contains("Total pages: 0"));
    let log = std::fs::read_to_string(root.join("log.md")).unwrap();
    assert!(log.contains("## ["));
    assert!(log.contains("Wiki initialized"));
}

#[tokio::test]
async fn boot_refuses_db_inside_vault_root() {
    // Threat T-01-01: craft a RuntimeConfig whose db_path lands inside
    // vault_root and assert boot fails with the actionable error.
    let tmp = TempDir::new().expect("tempdir");
    let vault_root = tmp.path().join("vaults/1");
    std::fs::create_dir_all(&vault_root).unwrap();
    let rt = RuntimeConfig {
        db_path: vault_root.join("saussunet.db"),
        vault_root: vault_root.clone(),
        bind: "127.0.0.1:8080".to_string(),
        vault_id: 1,
        vault_name: "main".to_string(),
        llm_provider: "mock".to_string(),
        llm_base_url: "https://api.openai.com/v1".to_string(),
        llm_model: String::new(),
        llm_api_key: None,
        llm_max_concurrency: 1,
        llm_timeout_secs: 300,
    };
    let err = boot(&rt).await.expect_err("must refuse db inside vault");
    assert!(err.to_string().contains("T-01-01"), "error: {err}");

    // Direct helper check (absolute path form).
    let db: PathBuf = vault_root.join("x.db");
    assert!(assert_outside_vault(&db, &vault_root).is_err());
}
