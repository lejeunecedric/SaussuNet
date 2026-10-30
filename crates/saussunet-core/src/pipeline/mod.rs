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

/// `ingest_pdf` payload schema (D-04): `{"raw_path": string, "source_id":
/// string, "filename": string}` — `raw_path` is the vault-relative path of
/// the immutable PDF copy written at submit time; `source_id` defaults to it.
#[derive(Debug, Clone, Deserialize)]
pub struct IngestPdfPayload {
    #[serde(default)]
    pub raw_path: String,
    #[serde(default)]
    pub source_id: String,
    #[serde(default)]
    pub filename: String,
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
        JOB_KIND_INGEST_PDF => {
            let payload: IngestPdfPayload = serde_json::from_str(&job.payload_json)
                .map_err(|e| CoreError::Queue(format!("ingest_pdf payload parse failed: {e}")))?;
            ingest_pdf(store, vault, provider, &payload).await
        }
        other => Err(CoreError::Queue(format!("unknown job kind: {other}"))),
    }
}

/// D-04 upload flow (called by the binary's upload route — the binary stays
/// transport-only, all guards live here): size cap → `%PDF-` magic + `.pdf`
/// extension → slugified filename (attacker-controlled, NEVER a path) →
/// byte-exact copy to `raw/papers/YYYY-MM-DD-slug.pdf` (immutable record) →
/// sha256 from the copy → insert the `ingest_pdf` job. Returns
/// `(job_id, raw_rel)`; the raw path also lives inside the job payload.
/// Drift is N/A for uploads (D-04).
pub async fn submit_pdf(
    store: &Store,
    vault: &Vault,
    bytes: &[u8],
    filename: &str,
    max_upload_bytes: u64,
) -> Result<(i64, String)> {
    // Defense in depth: the route caps the body too (DefaultBodyLimit), but
    // core never trusts its caller.
    if (bytes.len() as u64) > max_upload_bytes {
        return Err(CoreError::Vault(format!(
            "upload of {} bytes exceeds max_upload_bytes ({max_upload_bytes})",
            bytes.len()
        )));
    }
    if !bytes.starts_with(b"%PDF-") {
        return Err(CoreError::Vault(
            "upload rejected: not a PDF (missing %PDF- magic bytes)".to_string(),
        ));
    }
    if !filename.to_ascii_lowercase().ends_with(".pdf") {
        return Err(CoreError::Vault(
            "upload rejected: filename must end with .pdf".to_string(),
        ));
    }
    // Filename is attacker-controlled: strip the extension, then fold to
    // `[a-z0-9-]` — traversal is impossible by construction (write_raw's
    // sanitizer refuses anything that folds to empty).
    let stem = &filename[..filename.len() - 4];
    let slug = vault::raw::sanitize_component(stem)?;
    let pdf_path = vault::raw::write_raw_binary(vault, Some("papers"), &slug, "pdf", bytes).await?;
    // sha256 is taken from the COPY by the worker (D-04) and recorded on the
    // derived .md frontmatter + sync_state — the payload stays {raw_path,
    // source_id, filename} per the plan schema.
    let raw_rel = pdf_path
        .strip_prefix(&vault.root)
        .unwrap_or(&pdf_path)
        .to_string_lossy()
        .replace('\\', "/");
    let payload = serde_json::json!({
        "raw_path": raw_rel,
        "source_id": raw_rel,
        "filename": filename,
    });
    let job_id = store
        .insert_job(vault.id, JOB_KIND_INGEST_PDF, &payload.to_string())
        .await?;
    Ok((job_id, raw_rel))
}

/// D-04/D-06 worker half: read the raw PDF copy (path-guarded), extract text
/// with the pdftotext subprocess, land the derived text as a second raw file
/// (`raw/papers/YYYY-MM-DD-slug.md`, frontmatter source_url = the raw .pdf
/// path, sha256 = sha of the PDF copy) so provenance markers follow the same
/// D-07 shape as web pages — then run the SAME two-pass extraction as URLs.
async fn ingest_pdf(
    store: &Store,
    vault: &Arc<Vault>,
    provider: &Arc<dyn LlmProvider>,
    payload: &IngestPdfPayload,
) -> Result<()> {
    if payload.raw_path.trim().is_empty() {
        return Err(CoreError::Queue(
            "ingest_pdf payload missing raw_path".to_string(),
        ));
    }
    // The payload path is vault-relative and must resolve inside raw/ —
    // absolute paths and traversal are refused before any read.
    let rel = std::path::Path::new(&payload.raw_path);
    if rel.is_absolute() || !rel.starts_with("raw") {
        return Err(CoreError::Queue(format!(
            "ingest_pdf raw_path must be a relative path under raw/: {}",
            payload.raw_path
        )));
    }
    let pdf_path = vault.root.join(rel);
    if !pdf_path.starts_with(vault.root.join("raw")) {
        return Err(CoreError::Queue(format!(
            "ingest_pdf raw_path escapes raw/: {}",
            payload.raw_path
        )));
    }
    let bytes = tokio::fs::read(&pdf_path)
        .await
        .map_err(|e| CoreError::Io {
            path: pdf_path.clone(),
            source: e,
        })?;
    let sha256 = fetch::sha256_hex(&bytes);
    let text = extract::pdf_to_text(&pdf_path).await?;

    // Derived text file: reuse the PDF copy's slug (strip the date prefix) so
    // the pair reads `YYYY-MM-DD-slug.pdf` / `YYYY-MM-DD-slug.md`.
    let stem = pdf_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "paper".to_string());
    let date_prefix = format!("{}-", Utc::now().format("%Y-%m-%d"));
    let slug = stem.strip_prefix(&date_prefix).unwrap_or(&stem);
    let source_id = if payload.source_id.trim().is_empty() {
        payload.raw_path.clone()
    } else {
        payload.source_id.clone()
    };
    let md_path = vault::raw::write_raw(
        vault,
        Some("papers"),
        slug,
        "md",
        &vault::raw::RawFrontmatter {
            source_url: payload.raw_path.clone(),
            ingested: Utc::now().to_rfc3339(),
            sha256: sha256.clone(),
        },
        text.as_bytes(),
    )
    .await?;

    store
        .set_source_sha256(vault.id, &source_id, &sha256)
        .await?;
    let fetched = fetch::FetchResult {
        final_url: payload.raw_path.clone(),
        markdown: text,
        sha256,
        title: payload.filename.clone(),
    };
    run_extraction_passes(store, vault, provider, &md_path, &source_id, &fetched).await
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
