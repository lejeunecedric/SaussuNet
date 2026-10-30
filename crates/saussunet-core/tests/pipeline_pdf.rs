//! PDF slice end-to-end (plan 03-05, PIPE-01 / D-04 / D-06): upload guards in
//! `pipeline::submit_pdf`, the `ingest_pdf` worker (pdftotext subprocess →
//! derived raw .md → the SAME two-pass extraction as URLs), and the LOUD
//! missing-binary failure retained in `jobs.error`.
//!
//! CI requirement: the extraction e2e needs `poppler-utils` installed
//! (`pdftotext` on PATH); it self-skips with a loud eprintln when absent.
//! The missing-binary test simulates absence via `SAUSSUNET_PDFTOTEXT_PATH`
//! pointing at a nonexistent path (D-06: no silent fallback).

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tempfile::TempDir;

use saussunet_core::config::{FileConfig, RuntimeConfig};
use saussunet_core::llm::{LlmProvider, MockProvider, Scripted};
use saussunet_core::pipeline::{self, JOB_KIND_INGEST_PDF};
use saussunet_core::queue::{spawn_workers, WorkerConfig};
use saussunet_core::store::Job;
use saussunet_core::vault::Vault;
use saussunet_core::{boot_open, Store};

/// Minimal valid PDF (605 bytes) whose text object reads
/// "Saussure linguistics signifier" — verified with `pdftotext -q -layout`.
const PDF: &[u8] = include_bytes!("fixtures/sample.pdf");

/// Serializes tests that read/modify the process-wide
/// `SAUSSUNET_PDFTOTEXT_PATH` env var (tests in one binary run in parallel).
/// Async mutex: held across awaits inside the guarded tests.
static PDFTOTEXT_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn pdftotext_available() -> bool {
    std::process::Command::new("pdftotext")
        .arg("-v")
        .output()
        .is_ok()
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

fn today() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

/// The derived text file the ingest_pdf worker writes for `sample.pdf`.
fn derived_md_rel() -> String {
    format!("raw/papers/{}-sample.md", today())
}

fn fast_cfg() -> WorkerConfig {
    WorkerConfig {
        max_concurrency: 1,
        poll_interval: Duration::from_millis(10),
        heartbeat_interval: Duration::from_millis(50),
        claim_expiry: Duration::from_millis(300),
    }
}

/// Pass-2 page JSON with ≥2 whitelisted wikilinks + provenance to the real
/// derived raw/ file (PIPE-06 validator passes) — same shape as 03-03 tests.
fn page_json(title: &str, page_type: &str) -> serde_json::Value {
    json!({
        "title": title,
        "page_type": page_type,
        "tags": ["linguistics"],
        "summary": format!("{title} is a linguistic concept. ^[{}]", derived_md_rel()),
        "sections": [{
            "heading": "Overview",
            "markdown": format!("{title} anchors semiotics. ^[{}] See [[Meta]] and [[JavaScript]].", derived_md_rel())
        }],
        "contested": false,
        "contradictions": []
    })
}

async fn pending_job(store: &Store, vault_id: i64, job_id: i64) -> Job {
    for _ in 0..500 {
        let jobs = store.list_jobs(vault_id, None).await.expect("list");
        if let Some(j) = jobs.iter().find(|j| j.id == job_id) {
            return j.clone();
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("job {job_id} never appeared");
}

// ---------------------------------------------------------------------------
// submit_pdf guards (security gate: nothing lands in raw/ on rejection)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn submit_pdf_rejects_oversized_upload() {
    let (_tmp, vault_id, store, vault) = setup().await;
    let err = pipeline::submit_pdf(&store, &vault, PDF, "sample.pdf", 10)
        .await
        .expect_err("10-byte cap must reject the fixture");
    assert!(err.to_string().contains("max_upload_bytes"), "{err}");
    assert_eq!(store.list_jobs(vault_id, None).await.unwrap().len(), 0);
    // boot_open pre-creates raw/papers/, so assert nothing LANDED, not the dir.
    assert_eq!(
        std::fs::read_dir(vault.root.join("raw/papers"))
            .map(|d| d.count())
            .unwrap_or(0),
        0,
        "oversized upload must leave nothing in raw/papers/"
    );
}

#[tokio::test]
async fn submit_pdf_rejects_non_pdf_bytes() {
    let (_tmp, vault_id, store, vault) = setup().await;
    let err = pipeline::submit_pdf(&store, &vault, b"not a pdf", "doc.pdf", 1 << 20)
        .await
        .expect_err("missing %PDF- magic must be refused");
    assert!(err.to_string().to_lowercase().contains("pdf"), "{err}");
    assert_eq!(store.list_jobs(vault_id, None).await.unwrap().len(), 0);
}

#[tokio::test]
async fn submit_pdf_rejects_non_pdf_extension() {
    let (_tmp, _vault_id, store, vault) = setup().await;
    let err = pipeline::submit_pdf(&store, &vault, PDF, "evil.exe", 1 << 20)
        .await
        .expect_err("non-.pdf extension must be refused");
    assert!(err.to_string().to_lowercase().contains(".pdf"), "{err}");
}

#[tokio::test]
async fn submit_pdf_sanitizes_traversal_filename() {
    let (_tmp, vault_id, store, vault) = setup().await;
    let (job_id, raw_rel) = pipeline::submit_pdf(&store, &vault, PDF, "../../evil.pdf", 1 << 20)
        .await
        .expect("traversal filename must be slugified, not refused");
    assert_eq!(job_id, 1);
    // Stored path is inside raw/papers/ with a sanitized slug.
    assert!(
        raw_rel.starts_with(&format!("raw/papers/{}-", today())),
        "raw_rel: {raw_rel}"
    );
    assert!(!raw_rel.contains(".."), "traversal must be gone: {raw_rel}");
    assert!(
        raw_rel.ends_with("-evil.pdf"),
        "slug keeps the stem: {raw_rel}"
    );
    assert!(vault.root.join(&raw_rel).exists());
    // Nothing escaped raw/: the vault root has no top-level evil file.
    assert!(!vault.root.join("evil.pdf").exists());
    let job = pending_job(&store, vault_id, job_id).await;
    assert_eq!(job.kind, JOB_KIND_INGEST_PDF);
}

#[tokio::test]
async fn submit_pdf_happy_path_returns_job_id_and_immutable_copy() {
    let (_tmp, vault_id, store, vault) = setup().await;
    let (job_id, raw_rel) = pipeline::submit_pdf(&store, &vault, PDF, "sample.pdf", 1 << 20)
        .await
        .expect("valid upload accepted");
    assert_eq!(job_id, 1, "first inserted job id");
    assert_eq!(
        raw_rel,
        format!("raw/papers/{}-sample.pdf", today()),
        "D-07 naming under raw/papers/"
    );
    // D-04: the copy is the immutable record — byte-exact, written at SUBMIT time.
    let on_disk = std::fs::read(vault.root.join(&raw_rel)).expect("pdf copy");
    assert_eq!(on_disk, PDF, "byte-exact copy");
    // The job payload carries raw_path + source_id + filename.
    let job = pending_job(&store, vault_id, job_id).await;
    assert_eq!(job.kind, JOB_KIND_INGEST_PDF);
    let payload: serde_json::Value = serde_json::from_str(&job.payload_json).unwrap();
    assert_eq!(payload["raw_path"], raw_rel);
    assert_eq!(payload["source_id"], raw_rel);
    assert_eq!(payload["filename"], "sample.pdf");
}

// ---------------------------------------------------------------------------
// ingest_pdf worker: pdftotext → derived raw .md → shared two-pass extraction
// ---------------------------------------------------------------------------

/// PIPE-01: the ingest_pdf worker extracts text with pdftotext and runs the
/// SAME two-pass extraction as URLs — page on disk with provenance to the
/// raw PDF-derived text file. Self-skips (loud) when poppler is absent.
#[tokio::test]
async fn ingest_pdf_e2e_extracts_pages_with_provenance() {
    let _env_guard = PDFTOTEXT_ENV_LOCK.lock().await;
    if !pdftotext_available() {
        eprintln!("SKIP ingest_pdf_e2e: pdftotext not on PATH (CI requires poppler-utils)");
        return;
    }
    let (_tmp, vault_id, store, vault) = setup().await;
    let (job_id, _) = pipeline::submit_pdf(&store, &vault, PDF, "sample.pdf", 1 << 20)
        .await
        .expect("submit");
    let job = pending_job(&store, vault_id, job_id).await;

    let pass1 = json!({
        "candidates": [
            {"name": "Signifier", "kind": "concept", "mentions": 4, "central": true,
             "justification": "the PDF is about the signifier", "aliases": []}
        ]
    });
    let provider = Arc::new(MockProvider::new(vec![
        Scripted::Ok(pass1),
        Scripted::Ok(page_json("Signifier", "concept")),
    ]));
    let provider_dyn = provider as Arc<dyn LlmProvider>;
    pipeline::handle_job(&store, &vault, &provider_dyn, &job, "test-claim")
        .await
        .expect("handle_job ingest_pdf");

    // Derived text file exists with D-07 frontmatter pointing at the PDF copy.
    let md =
        std::fs::read_to_string(vault.root.join(derived_md_rel())).expect("derived raw .md exists");
    assert!(md.contains("Saussure linguistics signifier"), "{md}");
    assert!(
        md.contains(&format!("source_url: raw/papers/{}-sample.pdf", today())),
        "source_url = the raw pdf path: {md}"
    );

    // Page on disk with provenance to the derived raw .md (D-07 shape).
    let page = std::fs::read_to_string(vault.root.join("concepts/signifier.md"))
        .expect("page written by the shared two-pass pipeline");
    assert!(
        page.contains(&format!("^[{}]", derived_md_rel())),
        "provenance marker to the PDF-derived text: {page}"
    );
}

/// D-06 LOUD failure: a machine without pdftotext fails the job with the
/// actionable install hint retained in jobs.error — never a silent fallback.
#[tokio::test]
async fn missing_pdftotext_fails_loud_with_install_hint() {
    let _env_guard = PDFTOTEXT_ENV_LOCK.lock().await;
    let (_tmp, vault_id, store, vault) = setup().await;
    let (job_id, _) = pipeline::submit_pdf(&store, &vault, PDF, "sample.pdf", 1 << 20)
        .await
        .expect("submit");

    std::env::set_var("SAUSSUNET_PDFTOTEXT_PATH", "/nonexistent/pdftotext");
    let handles = spawn_workers(
        Arc::clone(&store),
        Arc::new(MockProvider::new(vec![])),
        Arc::clone(&vault),
        vault_id,
        fast_cfg(),
    );

    // First worker attempt must fail with the hint retained on the job row.
    let mut retained = None;
    for _ in 0..500 {
        let job = pending_job(&store, vault_id, job_id).await;
        if job.attempts >= 1 && job.error.is_some() {
            retained = job.error.clone();
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    for h in handles {
        h.abort();
    }
    std::env::remove_var("SAUSSUNET_PDFTOTEXT_PATH");

    let err = retained.expect("failed job must retain the error");
    assert!(
        err.contains("poppler-utils"),
        "D-06: jobs.error must carry the actionable hint, got: {err}"
    );
}
