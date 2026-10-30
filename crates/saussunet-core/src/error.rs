//! Typed error taxonomy for `saussunet-core` (PATTERNS §2.9 — no MoriNet
//! analog; established fresh). The library returns `crate::error::Result<T>`
//! so a future host can match on error kinds; `anyhow` stays at the binary
//! boundary (`crates/saussunet/src/main.rs`).

use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("config error: {0}")]
    Config(String),

    #[error("store error: {0}")]
    Store(#[from] sqlx::Error),

    #[error("io error at {path:?}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("vault error: {0}")]
    Vault(String),

    #[error("frontmatter error: {0}")]
    Frontmatter(String),

    #[error("write conflict on {path}: {reason}")]
    Conflict { path: String, reason: String },

    // Phase 2 (LLMP): LLM/queue variants. `Llm` deliberately carries ONLY
    // status + url — no body, no headers, no api_key — so nothing secret can
    // leak through Display/Debug into logs (D-03). Construct it explicitly;
    // there is intentionally no `#[from] reqwest::Error` conversion.
    #[error("LLM provider error {status} from {url}")]
    Llm { status: u16, url: String },

    #[error("LLM output not parseable as JSON after repair: {0}")]
    LlmParse(String),

    #[error("queue error: {0}")]
    Queue(String),

    // Phase 3 (PIPE-01): fetch/extraction errors. Like `Llm`, these carry
    // ONLY pre-sanitized strings — URLs stored here must already have had
    // userinfo stripped (D-03 pattern: nothing secret may ride Display into
    // logs).
    #[error("fetch error for {url}: {reason}")]
    Fetch { url: String, reason: String },

    #[error("blocked URL {url}: {reason}")]
    BlockedUrl { url: String, reason: String },
}

/// Convenience alias used across the core crate (never `anyhow` in the lib).
pub type Result<T> = std::result::Result<T, CoreError>;
