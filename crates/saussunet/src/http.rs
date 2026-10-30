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

use axum::extract::{DefaultBodyLimit, Multipart, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

use saussunet_core::error::CoreError;
use saussunet_core::pipeline::{self, JOB_KIND_INGEST_URL};
use saussunet_core::vault::Vault;
use saussunet_core::Store;

/// Shared handler state: a `RuntimeConfig` subset (store handle, target
/// vault, normalized base path, upload cap).
#[derive(Clone)]
pub struct AppState {
    pub store: Arc<Store>,
    pub vault: Arc<Vault>,
    pub vault_id: i64,
    pub base_path: String,
    /// `[pipeline].max_upload_bytes` (D-14) — enforced per-route on the
    /// upload endpoint AND inside `pipeline::submit_pdf` (defense in depth).
    pub max_upload_bytes: u64,
}

/// Build the full router: routes defined bare, then nested under the
/// normalized base path (D-13). Any other prefix 404s — no root fallback.
/// Body limits are PER-ROUTE: the JSON ingest keeps its 64 KiB cap while
/// the upload route gets the (much larger) `[pipeline].max_upload_bytes`.
pub fn build_router(state: AppState) -> Router {
    let base_path = state.base_path.clone();
    let upload_limit = state.max_upload_bytes as usize;
    let app = Router::new()
        .route("/health", get(health))
        .route("/ingest", post(ingest).layer(DefaultBodyLimit::max(65_536)))
        .route(
            "/ingest/upload",
            post(upload).layer(DefaultBodyLimit::max(upload_limit)),
        )
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

/// `POST {base}/ingest/upload` — multipart PDF upload (D-01/D-04). Transport
/// only: per-part checks (field `file`, content-type `application/pdf`,
/// `.pdf` filename, running size cap) then `pipeline::submit_pdf` does the
/// guarded copy into raw/ + job insert. Never logs bodies or filenames
/// (Phase 2 D-03 pattern: attacker-controlled strings stay out of logs).
async fn upload(State(state): State<AppState>, mut mp: Multipart) -> Response {
    let max = state.max_upload_bytes;
    let mut file: Option<(String, Vec<u8>)> = None;
    loop {
        let mut field = match mp.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => {
                // BodyLimit trips here on oversized bodies (413); anything
                // else is a malformed multipart body (its own status).
                let status = e.status();
                tracing::info!(method = "POST", path = %upload_path(&state), status = status.as_u16(), "upload rejected: multipart error");
                let _ = e; // error text can echo body fragments — not logged
                return (status, "invalid or oversized multipart body").into_response();
            }
        };
        if field.name() != Some("file") {
            continue;
        }
        let filename = field.file_name().unwrap_or("").to_string();
        if field.content_type() != Some("application/pdf") {
            tracing::info!(method = "POST", path = %upload_path(&state), status = 400, "upload rejected: wrong content-type");
            return (StatusCode::BAD_REQUEST, "file part must be application/pdf").into_response();
        }
        if !filename.to_ascii_lowercase().ends_with(".pdf") {
            tracing::info!(method = "POST", path = %upload_path(&state), status = 400, "upload rejected: wrong file extension");
            return (StatusCode::BAD_REQUEST, "filename must end with .pdf").into_response();
        }
        // Running cap while streaming — never buffer past max_upload_bytes.
        let mut bytes: Vec<u8> = Vec::new();
        loop {
            match field.chunk().await {
                Ok(Some(chunk)) => {
                    if bytes.len() + chunk.len() > max as usize {
                        tracing::info!(method = "POST", path = %upload_path(&state), status = 413, "upload rejected: oversized");
                        return (
                            StatusCode::PAYLOAD_TOO_LARGE,
                            format!("upload exceeds max_upload_bytes ({max})"),
                        )
                            .into_response();
                    }
                    bytes.extend_from_slice(&chunk);
                }
                Ok(None) => break,
                Err(e) => {
                    // BodyLimit trips here on oversized bodies (413).
                    let status = e.status();
                    tracing::info!(method = "POST", path = %upload_path(&state), status = status.as_u16(), "upload rejected: field read error");
                    let _ = e; // never logged — may carry body fragments
                    return (status, "upload read failed").into_response();
                }
            }
        }
        file = Some((filename, bytes));
        break; // first `file` part wins
    }
    let Some((filename, bytes)) = file else {
        tracing::info!(method = "POST", path = %upload_path(&state), status = 400, "upload rejected: missing file field");
        return (StatusCode::BAD_REQUEST, "missing file field named 'file'").into_response();
    };
    match pipeline::submit_pdf(&state.store, &state.vault, &bytes, &filename, max).await {
        Ok((job_id, raw_rel)) => {
            tracing::info!(method = "POST", path = %upload_path(&state), status = 202, job_id, "upload enqueued");
            (
                StatusCode::ACCEPTED,
                Json(serde_json::json!({ "job_id": job_id, "raw_path": raw_rel })),
            )
                .into_response()
        }
        Err(e @ CoreError::Store(_)) => {
            tracing::warn!(method = "POST", path = %upload_path(&state), status = 500, error = %e, "upload insert failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "failed to enqueue job").into_response()
        }
        Err(e) => {
            // Guard rejections (magic bytes, extension, cap) — Display is
            // pre-sanitized by core, safe to return as plain text.
            let status = if e.to_string().contains("max_upload_bytes") {
                StatusCode::PAYLOAD_TOO_LARGE
            } else {
                StatusCode::BAD_REQUEST
            };
            tracing::info!(method = "POST", path = %upload_path(&state), status = status.as_u16(), "upload rejected by guard");
            (status, e.to_string()).into_response()
        }
    }
}

/// The externally-visible upload path, for log lines only (never bodies).
fn upload_path(state: &AppState) -> String {
    format!("{}/ingest/upload", state.base_path)
}
