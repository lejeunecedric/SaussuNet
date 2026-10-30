//! Multipart upload route tests (plan 03-05, PIPE-01 / D-04 / D-01): POST
//! {base}/ingest/upload copies the PDF into raw/ AT SUBMIT TIME (before any
//! worker runs) and returns the job id; every guard rejection (wrong
//! content-type, non-PDF magic, oversized body) is a 4xx that lands nothing;
//! a traversal filename is stored sanitized inside raw/papers/. Router-level
//! via tower::ServiceExt::oneshot (no socket), same shape as http_ingest.rs.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tempfile::TempDir;
use tower::ServiceExt;

use saussunet::http::{build_router, AppState};
use saussunet_core::config::{FileConfig, RuntimeConfig};
use saussunet_core::vault::Vault;
use saussunet_core::{boot_open, Store};

/// Same fixture the core tests use (minimal valid PDF, "%PDF-" magic).
const PDF: &[u8] = include_bytes!("../../saussunet-core/tests/fixtures/sample.pdf");

fn multipart_body(boundary: &str, filename: &str, content_type: &str, bytes: &[u8]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    b.extend_from_slice(
        format!("content-disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\n")
            .as_bytes(),
    );
    b.extend_from_slice(format!("content-type: {content_type}\r\n\r\n").as_bytes());
    b.extend_from_slice(bytes);
    b.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    b
}

async fn upload_request(
    state: AppState,
    filename: &str,
    content_type: &str,
    bytes: &[u8],
) -> axum::response::Response {
    let body = multipart_body("----saussunet-test", filename, content_type, bytes);
    build_router(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/saussunet/ingest/upload")
                .header(
                    "content-type",
                    "multipart/form-data; boundary=----saussunet-test",
                )
                .body(Body::from(body))
                .expect("request builds"),
        )
        .await
        .expect("router responds")
}

/// `max_upload_bytes` override for the 413 test (route-level cap).
async fn setup(max_upload_bytes: Option<u64>) -> (TempDir, AppState) {
    let tmp = TempDir::new().expect("tempdir");
    let mut toml = format!(
        "[storage]\ndata_dir = \"{}\"\n\n[vault]\nid = 1\nname = \"main\"\n",
        tmp.path().display()
    );
    if let Some(cap) = max_upload_bytes {
        toml.push_str(&format!("\n[pipeline]\nmax_upload_bytes = {cap}\n"));
    }
    let cfg: FileConfig = toml::from_str(&toml).expect("test config parses");
    let rt: RuntimeConfig = cfg.resolve().expect("resolve");
    let (store, vault) = boot_open(&rt).await.expect("boot");
    let vault_id = vault.id;
    let state = AppState {
        store: Arc::new(store),
        vault: Arc::new(vault),
        vault_id,
        base_path: rt.base_path.clone(),
        max_upload_bytes: rt.pipeline_max_upload_bytes,
    };
    (tmp, state)
}

async fn body_text(resp: axum::response::Response) -> String {
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("body collects")
        .to_bytes();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn pdf_files_in(vault: &Vault) -> Vec<std::path::PathBuf> {
    let dir = vault.root.join("raw/papers");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "pdf"))
        .collect()
}

#[tokio::test]
async fn valid_upload_returns_202_job_row_and_pdf_on_disk_at_submit_time() {
    let (_tmp, state) = setup(None).await;
    let store: Arc<Store> = state.store.clone();
    let vault: Arc<Vault> = state.vault.clone();
    let vault_id = state.vault_id;

    let resp = upload_request(state, "sample.pdf", "application/pdf", PDF).await;
    assert_eq!(resp.status(), StatusCode::ACCEPTED, "valid PDF → 202");
    let json: serde_json::Value =
        serde_json::from_str(&body_text(resp).await).expect("202 body is JSON");
    assert_eq!(json["job_id"], 1, "body carries the new job id");
    assert!(
        json["raw_path"]
            .as_str()
            .is_some_and(|p| p.starts_with("raw/papers/") && p.ends_with(".pdf")),
        "body carries the raw path: {json}"
    );

    // D-04: the copy precedes the worker — it exists at SUBMIT time, and the
    // job row exists with kind ingest_pdf. No worker was ever spawned.
    let pdfs = pdf_files_in(&vault);
    assert_eq!(pdfs.len(), 1, "exactly one PDF landed in raw/papers/");
    assert_eq!(std::fs::read(&pdfs[0]).unwrap(), PDF, "byte-exact copy");
    let jobs = store.list_jobs(vault_id, None).await.expect("list_jobs");
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].kind, "ingest_pdf");
    assert_eq!(jobs[0].status, "pending");
}

#[tokio::test]
async fn wrong_content_type_is_400_and_lands_nothing() {
    let (_tmp, state) = setup(None).await;
    let store = state.store.clone();
    let vault = state.vault.clone();
    let vault_id = state.vault_id;
    let resp = upload_request(state, "sample.pdf", "text/plain", PDF).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert!(store.list_jobs(vault_id, None).await.unwrap().is_empty());
    assert!(pdf_files_in(&vault).is_empty());
}

#[tokio::test]
async fn non_pdf_magic_bytes_are_400_and_land_nothing() {
    let (_tmp, state) = setup(None).await;
    let store = state.store.clone();
    let vault = state.vault.clone();
    let vault_id = state.vault_id;
    let resp = upload_request(state, "doc.pdf", "application/pdf", b"not a pdf").await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert!(store.list_jobs(vault_id, None).await.unwrap().is_empty());
    assert!(pdf_files_in(&vault).is_empty());
}

#[tokio::test]
async fn oversized_upload_is_413_and_lands_nothing() {
    // Cap below the fixture size: the route-level DefaultBodyLimit (and the
    // running accumulation cap) must reject with 413.
    let (_tmp, state) = setup(Some(100)).await;
    let store = state.store.clone();
    let vault = state.vault.clone();
    let vault_id = state.vault_id;
    let resp = upload_request(state, "sample.pdf", "application/pdf", PDF).await;
    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert!(store.list_jobs(vault_id, None).await.unwrap().is_empty());
    assert!(pdf_files_in(&vault).is_empty());
}

#[tokio::test]
async fn traversal_filename_is_stored_sanitized_inside_raw_papers() {
    let (_tmp, state) = setup(None).await;
    let vault = state.vault.clone();
    let resp = upload_request(state, "../../x.pdf", "application/pdf", PDF).await;
    assert_eq!(
        resp.status(),
        StatusCode::ACCEPTED,
        "slugified, not refused"
    );
    let json: serde_json::Value =
        serde_json::from_str(&body_text(resp).await).expect("202 body is JSON");
    let raw_rel = json["raw_path"].as_str().unwrap();
    assert!(!raw_rel.contains(".."), "sanitized: {raw_rel}");
    assert!(
        raw_rel.starts_with("raw/papers/"),
        "inside raw/papers/: {raw_rel}"
    );
    assert!(
        raw_rel.ends_with("-x.pdf"),
        "slug keeps the stem: {raw_rel}"
    );
    assert!(vault.root.join(raw_rel).exists());
    // Nothing escaped raw/ — no x.pdf at the vault root or its parent.
    assert!(!vault.root.join("x.pdf").exists());
    assert!(!vault.root.join("-x.pdf").exists());
    assert!(vault
        .root
        .parent()
        .map(|p| !p.join("x.pdf").exists())
        .unwrap_or(true));
}
