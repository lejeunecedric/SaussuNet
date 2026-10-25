//! Human-edit protection end-to-end (VLET-03, ROADMAP SC3, D-05..D-08):
//! the PITFALLS "Looks Done But Isn't" recipe — hand-edit a generated page,
//! regenerate, and prove the edit survives (conflict recorded, never a
//! silent overwrite). Also covers protected refusal (D-07), human-owned
//! refusal including malformed frontmatter (D-08), clean regeneration, and
//! the conflicts listing the CLI renders.

use std::path::Path;
use std::sync::Arc;

use tempfile::TempDir;

use saussunet_core::config::{FileConfig, RuntimeConfig};
use saussunet_core::vault::ghash;
use saussunet_core::vault::writer::{write_page, WriteOutcome};
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

async fn setup() -> (TempDir, Arc<Vault>, Arc<Store>) {
    let tmp = TempDir::new().expect("tempdir");
    let rt = rt_for(tmp.path(), "main");
    let (store, vault) = boot_open(&rt).await.expect("boot");
    (tmp, Arc::new(vault), Arc::new(store))
}

/// A valid two-section llm-wiki page; `a_text` is the human-editable part
/// of the `## Overview` section.
fn page(title: &str, a_text: &str) -> String {
    format!(
        "---\ntitle: {title}\ncreated: 2020-01-01\nupdated: 2020-01-01\ntype: concept\ntags: [technology]\nsources: [raw/articles/src.md]\n---\n\nIntro for {title}.\n\n## Overview\n{a_text} See [[link-a]].\n\n## Details\nStable details. See [[link-b]].\n"
    )
}

fn read(vault: &Vault, rel: &str) -> String {
    std::fs::read_to_string(vault.root().join(rel)).expect("file present")
}

/// Insert a frontmatter line just before the closing `---` delimiter.
fn add_frontmatter_line(file: &str, line: &str) -> String {
    let (head, tail) = file.split_once("\n---\n").expect("frontmatter");
    format!("{head}\n{line}\n---\n{tail}")
}

async fn pending(store: &Store) -> Vec<saussunet_core::store::Proposal> {
    store.list_pending_proposals(1).await.expect("list")
}

#[tokio::test]
async fn human_edit_survives_regeneration() {
    // THE headline SC3 test (PITFALLS recipe: edit → re-ingest → survives).
    let (_tmp, vault, store) = setup().await;
    let rel = "concepts/gen-test.md";

    write_page(&vault, &store, rel, &page("Gen Test", "Section A v1."))
        .await
        .expect("first write");

    // gen_hash map present from the FIRST write (D-05, Pitfall 7).
    let file = read(&vault, rel);
    assert!(file.contains("gen_hash:"), "{file}");
    assert!(file.contains("__frontmatter__"));
    assert!(file.contains("overview:"));
    assert!(file.contains("details:"));

    // Simulate a human editing in Obsidian: hand-edit section A on disk.
    let edited = file.replace("Section A v1.", "HUMAN LINE — do not clobber");
    std::fs::write(vault.root().join(rel), &edited).expect("human edit");

    // Regenerate with different section A text.
    let outcome = write_page(
        &vault,
        &store,
        rel,
        &page("Gen Test", "Section A v2 regenerated."),
    )
    .await
    .expect("write_page");

    // The human's edit survives; the regenerated text never landed.
    let after = read(&vault, rel);
    assert!(after.contains("HUMAN LINE — do not clobber"), "{after}");
    assert!(!after.contains("Section A v2 regenerated"), "{after}");

    // Exactly one conflict row with the proposal content (D-06).
    let rows = pending(&store).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].reason, "gen_hash_mismatch");
    assert_eq!(rows[0].target_path, rel);
    assert!(!rows[0].proposed_content.is_empty());
    assert!(rows[0]
        .proposed_content
        .contains("Section A v2 regenerated"));

    // Outcome carries the proposal id (CLI exit-2 contract).
    match outcome {
        WriteOutcome::ConflictRecorded {
            reason,
            proposal_id,
        } => {
            assert_eq!(reason, "gen_hash_mismatch");
            assert_eq!(proposal_id, rows[0].id);
        }
        other => panic!("expected ConflictRecorded, got {other:?}"),
    }

    // log.md has the conflict entry.
    let log = read(&vault, "log.md");
    assert!(
        log.lines()
            .any(|l| l.starts_with("## [") && l.contains("conflict |")),
        "{log}"
    );
    assert!(log.contains("- skipped write to concepts/gen-test.md (human edit detected)"));
    assert!(log.contains(&format!(
        "- proposal recorded (pending_proposals #{})",
        rows[0].id
    )));
}

#[tokio::test]
async fn clean_regeneration_overwrites_and_rehashes() {
    let (_tmp, vault, store) = setup().await;
    let rel = "concepts/clean.md";

    write_page(&vault, &store, rel, &page("Clean", "Section A v1."))
        .await
        .expect("first write");
    let first = read(&vault, rel);

    // Round-trip guard: the stored map equals compute_map over the file as
    // read back — a freshly-written unedited page must never conflict.
    let (yaml, body) = saussunet_core::vault::frontmatter::split(&first);
    let fm = saussunet_core::vault::frontmatter::parse(yaml).unwrap();
    assert_eq!(
        fm.gen_hash,
        Some(ghash::compute_map(yaml, body)),
        "stored map must equal check-side map"
    );

    // Regenerate WITHOUT touching the file: clean overwrite.
    let outcome = write_page(&vault, &store, rel, &page("Clean", "Section A v2."))
        .await
        .expect("write_page");
    assert!(
        matches!(outcome, WriteOutcome::Written { .. }),
        "{outcome:?}"
    );

    let second = read(&vault, rel);
    assert!(second.contains("Section A v2."));
    assert!(!second.contains("Section A v1."));
    // gen_hash map updated (section A changed → its hash differs).
    let (yaml2, _) = saussunet_core::vault::frontmatter::split(&second);
    let fm2 = saussunet_core::vault::frontmatter::parse(yaml2).unwrap();
    assert_ne!(
        fm.gen_hash, fm2.gen_hash,
        "map must be rewritten from new content"
    );
    // updated bumped to today (create normalized it to today too; the
    // incoming 2020-01-01 input must NOT have leaked back in).
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    assert!(second.contains(&format!("updated: {today}")));
    assert!(
        second.contains(&format!("created: {today}")),
        "created normalized on create, preserved from the existing file on update"
    );

    assert!(pending(&store).await.is_empty(), "zero conflict rows");
}

#[tokio::test]
async fn protected_page_refuses_all_writes() {
    // D-07: hand-adopt a generated page via `protected: true`.
    let (_tmp, vault, store) = setup().await;
    let rel = "concepts/adopted.md";
    write_page(&vault, &store, rel, &page("Adopted", "Section A v1."))
        .await
        .expect("first write");

    let file = read(&vault, rel);
    let protected = add_frontmatter_line(&file, "protected: true");
    std::fs::write(vault.root().join(rel), &protected).expect("hand-adopt");
    let bytes_before = std::fs::read(vault.root().join(rel)).unwrap();

    let outcome = write_page(&vault, &store, rel, &page("Adopted", "Section A v2."))
        .await
        .expect("write_page");
    match outcome {
        WriteOutcome::Refused { reason } => assert_eq!(reason, "protected"),
        other => panic!("expected Refused, got {other:?}"),
    }

    // File bytes byte-identical before/after the refused write.
    let bytes_after = std::fs::read(vault.root().join(rel)).unwrap();
    assert_eq!(bytes_before, bytes_after);

    let rows = pending(&store).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].reason, "protected");
    assert_eq!(rows[0].target_path, rel);

    let log = read(&vault, "log.md");
    assert!(
        log.lines()
            .any(|l| l.starts_with("## [") && l.contains("refused |")),
        "{log}"
    );
    assert!(log.contains("(protected page)"));
}

#[tokio::test]
async fn human_owned_page_without_gen_hash_is_refused() {
    // D-08: a page under entities/ the writer never wrote is human-owned.
    let (_tmp, vault, store) = setup().await;
    let rel = "entities/human-note.md";
    let hand_made = page("Human Note", "Written by a person, not the agent.");
    std::fs::write(vault.root().join(rel), &hand_made).expect("hand-made page");
    let bytes_before = std::fs::read(vault.root().join(rel)).unwrap();

    let outcome = write_page(
        &vault,
        &store,
        rel,
        &page("Human Note", "Agent overwrite attempt."),
    )
    .await
    .expect("write_page");
    match outcome {
        WriteOutcome::Refused { reason } => assert_eq!(reason, "human_owned"),
        other => panic!("expected Refused, got {other:?}"),
    }

    let bytes_after = std::fs::read(vault.root().join(rel)).unwrap();
    assert_eq!(
        bytes_before, bytes_after,
        "human-owned file must be untouched"
    );

    let rows = pending(&store).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].reason, "human_owned");
    assert_eq!(rows[0].target_path, rel);

    let log = read(&vault, "log.md");
    assert!(log.contains("(human-owned page)"), "{log}");
}

#[tokio::test]
async fn frontmatter_only_human_edit_routes_to_conflict() {
    // D-05 separate-hash rule: a frontmatter-only edit is caught via the
    // __frontmatter__ key even though every section hash still matches.
    let (_tmp, vault, store) = setup().await;
    let rel = "concepts/fm-edit.md";
    write_page(&vault, &store, rel, &page("FM Edit", "Section A v1."))
        .await
        .expect("first write");

    let file = read(&vault, rel);
    let edited = add_frontmatter_line(&file, "confidence: high");
    std::fs::write(vault.root().join(rel), &edited).expect("human frontmatter edit");

    let outcome = write_page(&vault, &store, rel, &page("FM Edit", "Section A v2."))
        .await
        .expect("write_page");
    assert!(
        matches!(outcome, WriteOutcome::ConflictRecorded { .. }),
        "frontmatter-only edit must conflict: {outcome:?}"
    );
    let rows = pending(&store).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].reason, "gen_hash_mismatch");
    // The human's frontmatter line survives.
    assert!(read(&vault, rel).contains("confidence: high"));
}

#[tokio::test]
async fn conflicts_listing_matches_cli_contract() {
    let (_tmp, vault, store) = setup().await;
    let rel = "concepts/listed.md";

    // Empty vault: the CLI prints `no pending conflicts` (empty list).
    assert_eq!(store.count_pending(1).await.unwrap(), 0);
    assert!(store.list_pending_summary(1).await.unwrap().is_empty());

    // Reproduce test 1's state: write → human edit → regenerate.
    write_page(&vault, &store, rel, &page("Listed", "Section A v1."))
        .await
        .expect("first write");
    let file = read(&vault, rel);
    std::fs::write(
        vault.root().join(rel),
        file.replace("Section A v1.", "HUMAN LINE"),
    )
    .expect("human edit");
    write_page(&vault, &store, rel, &page("Listed", "Section A v2."))
        .await
        .expect("regenerate");

    // Summary listing (what the text CLI prints line-per-row).
    assert_eq!(store.count_pending(1).await.unwrap(), 1);
    let summaries = store.list_pending_summary(1).await.unwrap();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].target_path, rel);
    assert_eq!(summaries[0].reason, "gen_hash_mismatch");
    assert!(!summaries[0].created_at.is_empty());

    // --json path: full rows serialize as a JSON array with content.
    let rows = store.list_pending_proposals(1).await.unwrap();
    let json = serde_json::to_string(&rows).expect("serialize");
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("parses as JSON");
    let arr = parsed.as_array().expect("JSON array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["reason"], "gen_hash_mismatch");
    assert_eq!(arr[0]["target_path"], rel);
    assert!(arr[0]
        .get("proposed_content")
        .and_then(|c| c.as_str())
        .is_some_and(|c| !c.is_empty()));
}

#[tokio::test]
async fn malformed_frontmatter_under_entities_is_human_owned() {
    // D-08 gap: an UNPARSEABLE frontmatter file must never fall through to
    // the clean-overwrite branch — agent authorship cannot be proven.
    let (_tmp, vault, store) = setup().await;
    let rel = "entities/broken.md";
    let broken = "---\ntitle: [unclosed flow\n  bad: : yaml\n---\n\nHuman body with [[a]] and [[b]] links.\n";
    std::fs::write(vault.root().join(rel), broken).expect("broken page");
    let bytes_before = std::fs::read(vault.root().join(rel)).unwrap();

    let outcome = write_page(
        &vault,
        &store,
        rel,
        &page("Broken", "Agent overwrite attempt."),
    )
    .await
    .expect("write_page");
    match outcome {
        WriteOutcome::Refused { reason } => assert_eq!(reason, "human_owned"),
        other => panic!("expected Refused(human_owned), got {other:?}"),
    }

    let bytes_after = std::fs::read(vault.root().join(rel)).unwrap();
    assert_eq!(bytes_before, bytes_after, "file must be byte-identical");

    let rows = pending(&store).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].reason, "human_owned");
    assert_eq!(rows[0].target_path, rel);

    let log = read(&vault, "log.md");
    assert!(log.contains("(human-owned page)"), "{log}");
}
