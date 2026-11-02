//! `ingest_raw` end-to-end (plan 04-04, T15): a watched text file's raw copy
//! (written by `submit_raw` under `raw/watch/`) is drained by the worker pool
//! with MockProvider — zero real LLM — and runs the SAME extraction passes as
//! `ingest_pdf` (harness copied from tests/pipeline_pdf.rs).

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tempfile::TempDir;

use saussunet_core::config::{FileConfig, RuntimeConfig};
use saussunet_core::llm::{LlmProvider, MockProvider, Scripted};
use saussunet_core::pipeline::{self, JOB_KIND_INGEST_RAW};
use saussunet_core::queue::{spawn_workers, WorkerConfig};
use saussunet_core::vault::Vault;
use saussunet_core::{boot_open, Store};

fn today() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

async fn setup() -> (TempDir, i64, Arc<Store>, Arc<Vault>) {
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

/// Pass-2 page JSON with ≥2 whitelisted wikilinks + provenance to the raw
/// watch copy (PIPE-06 validator passes) — same shape as pipeline_pdf.rs.
fn page_json(raw_rel: &str) -> serde_json::Value {
    json!({
        "title": "Signifier",
        "page_type": "concept",
        "tags": ["linguistics"],
        "summary": format!("Signifier is a linguistic concept. ^[{raw_rel}]"),
        "sections": [{
            "heading": "Overview",
            "markdown": format!("Signifier anchors semiotics. ^[{raw_rel}] See [[Meta]] and [[JavaScript]].")
        }],
        "contested": false,
        "contradictions": []
    })
}

/// T15: submit_raw → worker pool (MockProvider) → job done → extraction
/// passes produced a page citing the raw/watch copy.
#[tokio::test]
async fn ingest_raw_end_to_end() {
    let (_tmp, vault_id, store, vault) = setup().await;
    let (job_id, raw_rel) = pipeline::submit_raw(
        &store,
        &vault,
        b"# Note\n\nMeta anchors JavaScript as a signifier.\n",
        "note.md",
        "http://webdav.invalid/note.md",
        1 << 20,
    )
    .await
    .expect("submit_raw");

    assert!(
        raw_rel.starts_with(&format!("raw/watch/{}-note.md", today())),
        "D-07 naming under raw/watch/: {raw_rel}"
    );
    // The raw copy carries provenance frontmatter (source_url = watched URL).
    let on_disk = std::fs::read_to_string(vault.root.join(&raw_rel)).expect("raw copy");
    assert!(
        on_disk.contains("source_url: http://webdav.invalid/note.md"),
        "{on_disk}"
    );
    assert!(on_disk.contains("sha256:"), "{on_disk}");

    // Payload shape mirrors IngestPdfPayload verbatim.
    let jobs = store.list_jobs(vault_id, Some("pending")).await.unwrap();
    let job = jobs.iter().find(|j| j.id == job_id).expect("pending job");
    assert_eq!(job.kind, JOB_KIND_INGEST_RAW);
    let payload: serde_json::Value = serde_json::from_str(&job.payload_json).unwrap();
    assert_eq!(payload["raw_path"], raw_rel);
    assert_eq!(payload["source_id"], raw_rel);
    assert_eq!(payload["filename"], "note.md");

    let pass1 = json!({
        "candidates": [
            {"name": "Signifier", "kind": "concept", "mentions": 4, "central": true,
             "justification": "the note is about the signifier", "aliases": []}
        ]
    });
    let provider = Arc::new(MockProvider::new(vec![
        Scripted::Ok(pass1),
        Scripted::Ok(page_json(&raw_rel)),
    ]));
    let handles = spawn_workers(
        Arc::clone(&store),
        provider,
        Arc::clone(&vault),
        vault_id,
        fast_cfg(),
    );

    let mut done = false;
    for _ in 0..500 {
        let jobs = store.list_jobs(vault_id, None).await.unwrap();
        let j = jobs.iter().find(|j| j.id == job_id).expect("job row");
        assert!(
            j.error.is_none() || j.status != "dead",
            "job dead-lettered: {:?}",
            j.error
        );
        if j.status == "done" {
            done = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    for h in handles {
        h.abort();
    }
    assert!(done, "ingest_raw job must reach status done");

    // The extraction passes ran: page on disk citing the raw watch copy.
    let page = std::fs::read_to_string(vault.root.join("concepts/signifier.md"))
        .expect("page written by the shared two-pass pipeline");
    assert!(
        page.contains(&format!("^[{raw_rel}]")),
        "provenance marker to the raw watch copy: {page}"
    );
}
