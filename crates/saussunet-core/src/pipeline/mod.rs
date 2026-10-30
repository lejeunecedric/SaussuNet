//! Extraction pipeline (Phase 3): durable job kinds + their handlers.
//!
//! Layout (03-01 plan decision): `fetch.rs` = SSRF-guarded URL fetch,
//! `extract.rs` = readability+htmd HTML→Markdown, `mod.rs` = job kind
//! constants, payload schemas, and the kind-dispatch handler called by
//! `queue::process_job`. D-02: one job covers a whole source end-to-end
//! (fetch → extract → raw write → LLM passes inside one handler), so a
//! crash resumes the source via queue re-claim.

pub mod extract;
pub mod fetch;
pub mod prompts;
pub mod schemas;

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use serde::Deserialize;

use crate::error::{CoreError, Result};
use crate::llm::LlmProvider;
use crate::store::{Job, Store};
use crate::vault::{self, Vault};

/// Job kinds registered with the durable queue (03-01 plan decision).
pub const JOB_KIND_INGEST_URL: &str = "ingest_url";
pub const JOB_KIND_INGEST_PDF: &str = "ingest_pdf";

/// `ingest_url` payload schema: `{"url": string, "source_id": string}`.
/// Unknown fields are ignored; `source_id` defaults to the URL verbatim.
#[derive(Debug, Clone, Deserialize)]
pub struct IngestUrlPayload {
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub source_id: String,
}

/// Fetch seam so tests can inject a stub fetch (the SSRF guard legitimately
/// blocks the loopback fixture servers a test would otherwise use). The
/// production implementation is [`HttpFetcher`]; the override below is the
/// documented test-only injection point.
#[async_trait]
pub trait Fetcher: Send + Sync {
    async fn fetch(&self, url: &str) -> Result<fetch::FetchResult>;
}

/// Production fetcher: SSRF guard → guarded reqwest fetch.
pub struct HttpFetcher {
    client: reqwest::Client,
}

impl HttpFetcher {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: fetch::default_client()?,
        })
    }
}

#[async_trait]
impl Fetcher for HttpFetcher {
    async fn fetch(&self, url: &str) -> Result<fetch::FetchResult> {
        let guarded = fetch::assert_public_http_url(url).await?;
        fetch::fetch_url(&self.client, &guarded).await
    }
}

/// TEST-ONLY injection point (03-01 plan Task 2): set a process-wide fetcher
/// override before spawning workers in an integration test. First call wins;
/// production code never calls this and falls back to [`HttpFetcher`].
#[doc(hidden)]
pub fn set_fetcher_override(f: Arc<dyn Fetcher>) {
    let _ = FETCHER_OVERRIDE.set(f);
}

static FETCHER_OVERRIDE: std::sync::OnceLock<Arc<dyn Fetcher>> = std::sync::OnceLock::new();

fn fetcher() -> Arc<dyn Fetcher> {
    FETCHER_OVERRIDE
        .get()
        .cloned()
        .unwrap_or_else(|| Arc::new(HttpFetcher::new().expect("reqwest client build")))
}

/// Kind dispatch called by `queue::process_job` (key link: queue → pipeline).
/// Unknown kinds fail loudly — `fail_job` dead-letters them after the retry
/// ladder with the error retained.
pub async fn handle_job(
    store: &Store,
    vault: &Arc<Vault>,
    provider: &Arc<dyn LlmProvider>,
    job: &Job,
    _claim_token: &str,
) -> Result<()> {
    match job.kind.as_str() {
        JOB_KIND_INGEST_URL => {
            let payload: IngestUrlPayload = serde_json::from_str(&job.payload_json)
                .map_err(|e| CoreError::Queue(format!("ingest_url payload parse failed: {e}")))?;
            ingest_url(store, vault, provider, &payload).await
        }
        JOB_KIND_INGEST_PDF => Err(CoreError::Queue(
            "ingest_pdf is not implemented until plan 03-05".to_string(),
        )),
        other => Err(CoreError::Queue(format!("unknown job kind: {other}"))),
    }
}

/// D-02 walking slice: one job covers the whole source — fetch → extract →
/// `raw/` write → (03-03: LLM passes) inside this single handler. A crash
/// mid-way resumes the whole source via queue re-claim.
async fn ingest_url(
    store: &Store,
    vault: &Arc<Vault>,
    provider: &Arc<dyn LlmProvider>,
    payload: &IngestUrlPayload,
) -> Result<()> {
    if payload.url.trim().is_empty() {
        return Err(CoreError::Queue(
            "ingest_url payload missing url".to_string(),
        ));
    }
    // source_id for URLs = the submitted URL string verbatim (plan decision).
    let source_id = if payload.source_id.trim().is_empty() {
        payload.url.clone()
    } else {
        payload.source_id.clone()
    };
    let result = fetcher().fetch(&payload.url).await?;
    let slug_src = if result.title.trim().is_empty() {
        url_host(&result.final_url)
    } else {
        result.title.clone()
    };
    let raw_path = vault::raw::write_raw(
        vault,
        None,
        &slug_src,
        "md",
        &vault::raw::RawFrontmatter {
            source_url: result.final_url.clone(),
            ingested: Utc::now().to_rfc3339(),
            sha256: result.sha256.clone(),
        },
        result.markdown.as_bytes(),
    )
    .await?;
    // D-08: the digest lands in its own sync_state.sha256 column
    // (migration 0007 — 03-01's `token` placeholder is retired; the
    // migration backfilled any rows written before this change).
    store
        .set_source_sha256(vault.id, &source_id, &result.sha256)
        .await?;
    run_extraction_passes(store, vault, provider, &raw_path, &result).await
}

/// TODO(03-03): two-pass LLM extraction seam (D-09: pass 1 candidates,
/// pass 2 page generation/merge). No-op in the 03-01 walking skeleton.
async fn run_extraction_passes(
    _store: &Store,
    _vault: &Arc<Vault>,
    _provider: &Arc<dyn LlmProvider>,
    _raw_path: &std::path::Path,
    _fetched: &fetch::FetchResult,
) -> Result<()> {
    Ok(())
}

fn url_host(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_string()))
        .unwrap_or_else(|| "source".to_string())
}
