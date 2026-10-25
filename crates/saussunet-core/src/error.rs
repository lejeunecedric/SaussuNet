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
}

/// Convenience alias used across the core crate (never `anyhow` in the lib).
pub type Result<T> = std::result::Result<T, CoreError>;
