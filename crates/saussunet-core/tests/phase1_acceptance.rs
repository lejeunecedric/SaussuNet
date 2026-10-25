//! Phase-1 acceptance test: all five ROADMAP success criteria asserted
//! together on ONE fresh temp data dir, goal-backward, in order. Each block
//! quotes its criterion verbatim from `.planning/ROADMAP.md` §Phase 1.

use std::path::Path;

use tempfile::TempDir;

use saussunet_core::boot_open;
use saussunet_core::config::{FileConfig, RuntimeConfig};
use saussunet_core::vault::frontmatter::{self, count_outbound_wikilinks};
use saussunet_core::vault::writer::{write_page, WriteOutcome};
use saussunet_core::vault::Vault;

fn rt_for(data_dir: &Path) -> RuntimeConfig {
    let toml = format!(
        "[storage]\ndata_dir = \"{}\"\n\n[vault]\nid = 1\nname = \"main\"\n",
        data_dir.display()
    );
    let cfg: FileConfig = toml::from_str(&toml).expect("test config parses");
    cfg.resolve()
}

fn page(title: &str, overview: &str) -> String {
    format!(
        "---\ntitle: {title}\ncreated: 2020-01-01\nupdated: 2020-01-01\ntype: concept\ntags: [technology]\nsources: [raw/articles/src.md]\n---\n\nIntro for {title}.\n\n## Overview\n{overview} See [[link-a]].\n\n## Details\nStable details. See [[link-b]].\n"
    )
}

fn read(vault: &Vault, rel: &str) -> String {
    std::fs::read_to_string(vault.root().join(rel)).expect("file present")
}

/// Recursively count files whose name contains `.tmp-` (writer temp leak).
fn count_tmp_files(dir: &Path) -> usize {
    let mut n = 0;
    for entry in std::fs::read_dir(dir).expect("read_dir") {
        let entry = entry.expect("entry");
        let path = entry.path();
        if entry.file_type().unwrap().is_dir() {
            n += count_tmp_files(&path);
        } else if entry.file_name().to_string_lossy().contains(".tmp-") {
            n += 1;
        }
    }
    n
}

/// The documented frontmatter key set (SCHEMA.md contract): a page openable
/// "in Obsidian unchanged" must not carry non-standard top-level keys.
const DOCUMENTED_KEYS: [&str; 11] = [
    "title",
    "created",
    "updated",
    "type",
    "tags",
    "sources",
    "confidence",
    "contested",
    "contradictions",
    "protected",
    "gen_hash",
];

#[tokio::test]
async fn phase1_success_criteria_end_to_end() {
    let tmp = TempDir::new().expect("tempdir");
    let rt = rt_for(tmp.path());
    let (store, vault) = boot_open(&rt).await.expect("boot");

    // ---------------------------------------------------------------- SC1
    // "Running the binary creates a vault with SCHEMA.md, index.md, log.md,
    // an immutable raw/ layer, and pages with full YAML frontmatter and ≥2
    // outbound wikilinks — openable in Obsidian unchanged"
    for seed in ["SCHEMA.md", "index.md", "log.md"] {
        assert!(vault.root().join(seed).is_file(), "{seed} missing");
    }
    for raw in [
        "raw/articles",
        "raw/papers",
        "raw/transcripts",
        "raw/assets",
    ] {
        assert!(vault.root().join(raw).is_dir(), "{raw} missing");
    }
    write_page(&vault, &store, "concepts/sc1.md", &page("SC1 Page", "v1."))
        .await
        .expect("SC1 write");
    let sc1_file = read(&vault, "concepts/sc1.md");
    let (sc1_yaml, sc1_body) = frontmatter::split(&sc1_file);
    let sc1_fm = frontmatter::parse(sc1_yaml).expect("SC1 frontmatter parses");
    assert_eq!(sc1_fm.title, "SC1 Page");
    assert_eq!(sc1_fm.page_type, "concept");
    assert!(!sc1_fm.created.is_empty() && !sc1_fm.updated.is_empty());
    assert!(!sc1_fm.tags.is_empty() && !sc1_fm.sources.is_empty());
    assert!(sc1_fm.gen_hash.is_some(), "recorded from the first write");
    assert!(count_outbound_wikilinks(sc1_body) >= 2, "≥2 wikilinks");
    // Standard YAML only: no top-level keys beyond the documented set.
    let doc: serde_yml::Mapping =
        serde_yml::from_str(sc1_yaml).expect("frontmatter is a YAML mapping");
    // serde_yml Mapping is String-keyed (01-03 lesson).
    for key in doc.keys() {
        assert!(
            DOCUMENTED_KEYS.contains(&key.as_str()),
            "non-standard frontmatter key {key:?}"
        );
    }

    // ---------------------------------------------------------------- SC2
    // "The vault-writer component is the only author of vault files and
    // every write is atomic (temp + rename): killing the process mid-write
    // never leaves a half-written page" — observable leg: zero temp leftovers
    // and every page file parses (no torn frontmatter). The writer-is-sole-
    // author invariant is structural (01-02: sole mutation entry, grep audit
    // in check.sh-era tests); restated here as the no-temps observable.
    assert_eq!(count_tmp_files(vault.root()), 0, "zero .tmp-* files");
    for rel in ["concepts/sc1.md", "index.md", "log.md", "SCHEMA.md"] {
        let content = read(&vault, rel);
        let (yaml, _) = frontmatter::split(&content);
        if rel != "index.md" && rel != "log.md" && rel != "SCHEMA.md" {
            frontmatter::parse(yaml).unwrap_or_else(|e| panic!("{rel} torn: {e}"));
        }
        assert!(!content.is_empty(), "{rel} non-empty");
    }

    // ---------------------------------------------------------------- SC3
    // "A human edit made to a generated page survives the next regeneration
    // (three-way merge via per-section gen_hash recorded from the first
    // write)" — same recipe as tests/gen_hash.rs, re-run end-to-end here.
    write_page(
        &vault,
        &store,
        "concepts/sc3.md",
        &page("SC3 Page", "Section A v1."),
    )
    .await
    .expect("SC3 first write");
    let edited =
        read(&vault, "concepts/sc3.md").replace("Section A v1.", "HUMAN LINE — do not clobber");
    std::fs::write(vault.root().join("concepts/sc3.md"), &edited).expect("human edit");
    let outcome = write_page(
        &vault,
        &store,
        "concepts/sc3.md",
        &page("SC3 Page", "Section A v2."),
    )
    .await
    .expect("SC3 regeneration");
    assert!(
        matches!(
            outcome,
            WriteOutcome::ConflictRecorded {
                ref reason,
                ..
            } if reason == "gen_hash_mismatch"
        ),
        "{outcome:?}"
    );
    let after = read(&vault, "concepts/sc3.md");
    assert!(after.contains("HUMAN LINE — do not clobber"));
    assert!(!after.contains("Section A v2."));
    let conflicts = store.list_pending_summary(vault.id).await.unwrap();
    assert_eq!(conflicts.len(), 1, "conflict row exists");
    assert_eq!(conflicts[0].reason, "gen_hash_mismatch");

    // SC2 re-check after the SC3 writes: still zero temp leftovers.
    assert_eq!(count_tmp_files(vault.root()), 0, "zero .tmp-* files");

    // ---------------------------------------------------------------- SC4
    // "Config, SQLite schema, paths, and queues all carry a vault id, even
    // though v1 ships exactly one vault"
    assert_eq!(rt.vault_id, 1, "config carries the vault id");
    assert!(
        rt.vault_root.ends_with(Path::new("vaults/1")),
        "paths keyed by vault id: {:?}",
        rt.vault_root
    );
    // Schema: every table carries the vault id (`id` on vaults itself).
    for table in [
        "vaults",
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
            .expect("table_info");
        let names: Vec<&str> = cols.iter().map(|(_, n)| n.as_str()).collect();
        let wanted = if table == "vaults" { "id" } else { "vault_id" };
        assert!(names.contains(&wanted), "{table} lacks {wanted}: {names:?}");
    }
    // Queue leg: a jobs row is only visible under its own vault id.
    store.ensure_vault(2, "second").await.expect("vault 2");
    store
        .insert_job(rt.vault_id, "ingest", "{}")
        .await
        .expect("insert_job");
    assert_eq!(store.list_jobs(rt.vault_id, None).await.unwrap().len(), 1);
    assert!(
        store.list_jobs(2, None).await.unwrap().is_empty(),
        "jobs are vault-scoped"
    );

    // ---------------------------------------------------------------- SC5
    // "The binary runs with SQLite in WAL mode, no external services, and
    // binds to localhost by default"
    let (mode,): (String,) = sqlx::query_as("PRAGMA journal_mode")
        .fetch_one(store.pool())
        .await
        .expect("journal_mode");
    assert_eq!(mode.to_lowercase(), "wal", "WAL on the live DB");
    assert!(
        rt.bind.starts_with("127.0.0.1"),
        "localhost bind default: {}",
        rt.bind
    );
    // Zero external services / zero listeners is structural: this test never
    // starts a server and `boot_open` returned without opening any socket
    // (Phase 1 opens no listener at all). Per SKELETON.md this localhost-bind
    // reading of SC5 is a sanctioned planner-discretion decision
    // (01-CONTEXT.md "Claude's Discretion"): the Phase-1 leg of SC5 is
    // WAL + zero external services + a localhost bind *default* in the
    // resolved config — asserted above.

    // Final sweep: the whole test left zero temp files behind.
    assert_eq!(count_tmp_files(vault.root()), 0);
}
