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
pub mod pages;
pub mod prompts;
pub mod schemas;

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use serde::Deserialize;

use crate::error::{CoreError, Result};
use crate::llm::{generate_json_typed, LlmProvider};
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

    // PIPE-02 / D-08: unchanged-source no-op. The digest comparison happens
    // after fetch (sha256 is only known post-fetch); "skip" means skipping the
    // EXTRACTION half — fetch is cheap, the LLM is expensive. When the digest
    // matches sync_state the job completes as a no-op: zero LLM calls, zero
    // vault writes (Pitfall 1 token-burn bound).
    let prev = store.get_source_sha256(vault.id, &source_id).await?;
    if prev.as_deref() == Some(result.sha256.as_str()) {
        tracing::info!(source_id = %source_id, "unchanged source — no-op");
        return Ok(());
    }

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

    // D-08 drift: same source, new digest → loud log.md entry before the
    // re-extraction (first-seen sources, prev == None, log nothing extra).
    // Subject is sanitized with writer.rs's log-injection guard (T-02-05);
    // the entry carries only URL/title + old/new digests, never page content.
    if let Some(old_sha) = prev {
        let subject = if result.title.trim().is_empty() {
            payload.url.as_str()
        } else {
            result.title.as_str()
        };
        let drift_raw_rel = raw_path
            .strip_prefix(&vault.root)
            .unwrap_or(&raw_path)
            .to_string_lossy()
            .replace('\\', "/");
        vault::log::append_entry(
            vault,
            &format!(
                "## [{}] drift | {}",
                Utc::now().format("%Y-%m-%d"),
                crate::vault::writer::sanitize_for_nav(subject)
            ),
            &[
                format!("- sha256 {old_sha} -> {}", result.sha256),
                format!("- re-ingested {drift_raw_rel}"),
            ],
        )
        .await?;
    }

    run_extraction_passes(store, vault, provider, &raw_path, &source_id, &result).await?;
    // D-08: the digest lands in its own sync_state.sha256 column
    // (migration 0007 — 03-01's `token` placeholder is retired). Stored
    // AFTER extraction succeeds (03-04 move): a job that crashed mid-extraction
    // must re-ingest on re-claim rather than silently no-op next time (D-02).
    store
        .set_source_sha256(vault.id, &source_id, &result.sha256)
        .await?;
    Ok(())
}

/// Page-creation threshold default (D-14): "2+ sources". The resolved
/// `RuntimeConfig.pipeline_min_sources` lands here once the queue handler
/// signature carries config; `pages::gate_and_write` takes the value as a
/// parameter so plumbing it is a one-line change at the call below.
pub const DEFAULT_MIN_SOURCES: u32 = 2;

/// D-09: the two LLM passes per source. Pass 1 extracts typed candidates
/// (centrality + justification + aliases) seeing the registry index;
/// `pages::gate_and_write` applies the threshold and runs pass 2 per
/// eligible page (existing page + new evidence → merged content). A pass-1
/// failure propagates — the queue retry ladder re-runs the whole source
/// (D-02), which is safe: raw write is same-day overwrite and source-set
/// inserts are idempotent.
async fn run_extraction_passes(
    store: &Store,
    vault: &Arc<Vault>,
    provider: &Arc<dyn LlmProvider>,
    raw_path: &std::path::Path,
    source_id: &str,
    fetched: &fetch::FetchResult,
) -> Result<()> {
    // Provenance + sources frontmatter use the vault-relative raw path.
    let raw_rel = raw_path
        .strip_prefix(&vault.root)
        .unwrap_or(raw_path)
        .to_string_lossy()
        .replace('\\', "/");

    // Registry index for pass 1 (D-15/D-16): canonical names + aliases, so
    // the LLM reuses existing names instead of minting near-duplicates.
    let registry_index = if let Ok(entities) = store.list_entities(vault.id).await {
        if entities.is_empty() {
            "(none yet)".to_string()
        } else {
            entities
                .iter()
                .map(|e| {
                    let aliases: Vec<String> =
                        serde_json::from_str(&e.aliases_json).unwrap_or_default();
                    if aliases.is_empty() {
                        format!("- {}", e.canonical_name)
                    } else {
                        format!("- {} (aliases: {})", e.canonical_name, aliases.join(", "))
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
    } else {
        "(registry unavailable)".to_string()
    };

    let prompt = prompts::pass1_prompt(&raw_rel, &fetched.markdown, &registry_index);
    tracing::debug!(
        "pipeline pass-1 prompt: {}",
        prompt.chars().take(400).collect::<String>()
    );
    let pass1: schemas::ExtractCandidates = generate_json_typed(
        provider.as_ref(),
        &prompt,
        &schemas::ExtractCandidates::json_schema_hint(),
    )
    .await?;
    tracing::debug!(
        "pipeline pass-1 output: {} candidates",
        pass1.candidates.len()
    );

    pages::gate_and_write(
        store,
        vault,
        provider,
        DEFAULT_MIN_SOURCES,
        &raw_rel,
        source_id,
        &fetched.markdown,
        &pass1,
    )
    .await?;
    Ok(())
}

fn url_host(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_string()))
        .unwrap_or_else(|| "source".to_string())
}
