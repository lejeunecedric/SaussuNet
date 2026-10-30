//! Router-level tests for the Phase 3 HTTP surface (plan 03-02): `POST
//! {base}/ingest` enqueues a durable `ingest_url` job and returns its id
//! (D-01); every route lives under the configured base path (D-13); blocked
//! (SSRF) or malformed submissions are rejected with 4xx and insert NO job
//! row. Tests drive the router directly via `tower::ServiceExt::oneshot`
//! (no socket) except the final real-bind smoke test.

use std::path::Path;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tempfile::TempDir;
use tower::ServiceExt;

use saussunet::http::{build_router, AppState};
use saussunet_core::config::{FileConfig, RuntimeConfig};
use saussunet_core::{boot_open, Store};

/// Same file-backed fixture as core tests/queue.rs (file-backed DB required).
fn rt_for(data_dir: &Path, name: &str) -> RuntimeConfig {
    let toml = format!(
        "[storage]\ndata_dir = \"{}\"\n\n[vault]\nid = 1\nname = \"{name}\"\n",
        data_dir.display()
    );
    let cfg: FileConfig = toml::from_str(&toml).expect("test config parses");
    cfg.resolve().expect("resolve")
}

async fn setup() -> (TempDir, AppState) {
    let tmp = TempDir::new().expect("tempdir");
    let rt = rt_for(tmp.path(), "main");
    let (store, vault) = boot_open(&rt).await.expect("boot");
    let state = AppState {
        store: Arc::new(store),
        vault_id: vault.id,
        base_path: rt.base_path.clone(),
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

#[tokio::test]
async fn post_ingest_valid_url_returns_202_and_inserts_pending_job() {
    let (_tmp, state) = setup().await;
    let vault_id = state.vault_id;
    let store: Arc<Store> = state.store.clone();
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/saussunet/ingest")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"url":"http://93.184.216.34/page"}"#))
                .expect("request builds"),
        )
        .await
        .expect("router responds");

    assert_eq!(resp.status(), StatusCode::ACCEPTED, "valid URL → 202");
    let json: serde_json::Value =
        serde_json::from_str(&body_text(resp).await).expect("202 body is JSON");
    assert_eq!(json["job_id"], 1, "body must carry the new job id");

    let jobs = store
        .list_jobs(vault_id, Some("pending"))
        .await
        .expect("list_jobs");
    assert_eq!(jobs.len(), 1, "exactly one pending job row");
    assert_eq!(jobs[0].kind, "ingest_url");
    assert!(
        jobs[0].payload_json.contains("93.184.216.34"),
        "payload must carry the submitted URL: {}",
        jobs[0].payload_json
    );
}

#[tokio::test]
async fn post_ingest_without_base_path_is_404() {
    // D-13: the endpoint exists ONLY under the configured base path.
    let (_tmp, state) = setup().await;
    let vault_id = state.vault_id;
    let store = state.store.clone();
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/ingest")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"url":"http://93.184.216.34/page"}"#))
                .expect("request builds"),
        )
        .await
        .expect("router responds");

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert!(
        store
            .list_jobs(vault_id, None)
            .await
            .expect("list_jobs")
            .is_empty(),
        "a 404 must not insert a job row"
    );
}

#[tokio::test]
async fn post_ingest_blocked_url_is_400_and_inserts_no_job() {
    // SSRF defense-in-depth at submit time (the authoritative guard stays in
    // the worker fetch path). 169.254.169.254 is the canonical metadata IP.
    let (_tmp, state) = setup().await;
    let vault_id = state.vault_id;
    let store = state.store.clone();
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/saussunet/ingest")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"url":"http://169.254.169.254/"}"#))
                .expect("request builds"),
        )
        .await
        .expect("router responds");

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert!(
        store
            .list_jobs(vault_id, None)
            .await
            .expect("list_jobs")
            .is_empty(),
        "a rejected URL must not insert a job row"
    );
}

#[tokio::test]
async fn post_ingest_malformed_body_is_400() {
    let (_tmp, state) = setup().await;
    let vault_id = state.vault_id;
    let store = state.store.clone();
    let app = build_router(state);

    // Not JSON at all.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/saussunet/ingest")
                .header("content-type", "application/json")
                .body(Body::from("not json"))
                .expect("request builds"),
        )
        .await
        .expect("router responds");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "bad JSON → 400");

    // Valid JSON but missing the `url` field.
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/saussunet/ingest")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"nope":true}"#))
                .expect("request builds"),
        )
        .await
        .expect("router responds");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "missing url → 400");

    assert!(
        store
            .list_jobs(vault_id, None)
            .await
            .expect("list_jobs")
            .is_empty(),
        "malformed bodies must not insert job rows"
    );
}

#[tokio::test]
async fn health_under_base_path_is_200_ok() {
    let (_tmp, state) = setup().await;
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/saussunet/health")
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("router responds");

    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_text(resp).await, "ok");
}

#[tokio::test]
async fn real_bind_serves_health_over_loopback() {
    // One real-socket proof that axum::serve + the nested router work
    // end-to-end on 127.0.0.1 (T-01-02 posture: bind stays loopback).
    let (_tmp, state) = setup().await;
    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind 127.0.0.1:0");
    let addr = listener.local_addr().expect("local addr");
    let serve = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });

    let resp = reqwest::get(format!("http://{addr}/saussunet/health"))
        .await
        .expect("reqwest GET health");
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.expect("body"), "ok");

    serve.abort();
}
