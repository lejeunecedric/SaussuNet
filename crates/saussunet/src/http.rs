//! The daemon's HTTP surface (plan 03-02, D-01/D-13). The ONLY axum code in
//! the repo — `saussunet-core` stays axum-free (Anti-Pattern 1).
//!
//! Threat-model posture (plan 03-02):
//! - The endpoint NEVER fetches and NEVER writes to the vault: its only side
//!   effect is `store.insert_job`. The authoritative SSRF guard stays in the
//!   worker fetch path; `assert_public_http_url` here is submit-time defense
//!   in depth so obviously-blocked URLs fail fast with 400.
//! - Logging carries method, path, status and job id only — never bodies.
//! - The JSON body is capped at 64 KiB (`DefaultBodyLimit`): this endpoint
//!   accepts only a URL string (upload guards land in 03-05).

use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

use saussunet_core::pipeline::{self, JOB_KIND_INGEST_URL};
use saussunet_core::Store;

/// Shared handler state: a `RuntimeConfig` subset (store handle, target
/// vault, normalized base path).
#[derive(Clone)]
pub struct AppState {
    pub store: Arc<Store>,
    pub vault_id: i64,
    pub base_path: String,
}

/// Build the full router: routes defined bare, then nested under the
/// normalized base path (D-13). Any other prefix 404s — no root fallback.
pub fn build_router(state: AppState) -> Router {
    let base_path = state.base_path.clone();
    let app = Router::new()
        .route("/health", get(health))
        .route("/ingest", post(ingest).layer(DefaultBodyLimit::max(65_536)))
        .with_state(state);
    Router::new().nest(&base_path, app)
}

async fn health() -> &'static str {
    "ok"
}

/// `POST {base}/ingest` body schema: `{"url": "..."}`.
#[derive(Deserialize)]
struct IngestReq {
    url: String,
}

async fn ingest(State(state): State<AppState>, body: String) -> Response {
    let req: IngestReq = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(e) => {
            tracing::info!(method = "POST", path = %ingest_path(&state), status = 400, "ingest rejected: bad body");
            return (StatusCode::BAD_REQUEST, format!("invalid JSON body: {e}")).into_response();
        }
    };
    if req.url.trim().is_empty() {
        tracing::info!(method = "POST", path = %ingest_path(&state), status = 400, "ingest rejected: empty url");
        return (StatusCode::BAD_REQUEST, "url must not be empty").into_response();
    }
    // Submit-time SSRF check (defense in depth — the worker re-guards at
    // fetch time). Error Display is pre-sanitized by the guard (03-01).
    if let Err(e) = pipeline::fetch::assert_public_http_url(&req.url).await {
        tracing::info!(method = "POST", path = %ingest_path(&state), status = 400, "ingest rejected: blocked url");
        return (StatusCode::BAD_REQUEST, format!("url rejected: {e}")).into_response();
    }

    let payload = serde_json::json!({ "url": req.url, "source_id": req.url });
    match state
        .store
        .insert_job(state.vault_id, JOB_KIND_INGEST_URL, &payload.to_string())
        .await
    {
        Ok(id) => {
            tracing::info!(method = "POST", path = %ingest_path(&state), status = 202, job_id = id, "ingest enqueued");
            (
                StatusCode::ACCEPTED,
                Json(serde_json::json!({ "job_id": id })),
            )
                .into_response()
        }
        Err(e) => {
            tracing::warn!(method = "POST", path = %ingest_path(&state), status = 500, error = %e, "ingest insert failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "failed to enqueue job").into_response()
        }
    }
}

/// The externally-visible ingest path, for log lines only (never bodies).
fn ingest_path(state: &AppState) -> String {
    format!("{}/ingest", state.base_path)
}
