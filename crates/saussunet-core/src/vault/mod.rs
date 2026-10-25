//! Vault layer: `Vault` (FS boundary keyed by vault id, D-04) + first-boot
//! FS initialization. The single-writer lock and atomic writes land with the
//! writer plan; this module owns directory/seed creation only.

pub mod index;
pub mod log;
pub mod schema;

use std::path::{Path, PathBuf};

use crate::error::{CoreError, Result};

/// Directory skeleton: `raw/` subdirs per the llm-wiki skill Architecture
/// (immutable evidence layer) + the four writable page sections.
const DIRECTORIES: [&str; 8] = [
    "raw/articles",
    "raw/papers",
    "raw/transcripts",
    "raw/assets",
    "entities",
    "concepts",
    "comparisons",
    "queries",
];

/// A vault on disk. `root` is always `data/vaults/<db row id>` (D-04).
#[derive(Debug, Clone)]
pub struct Vault {
    pub id: i64,
    pub name: String,
    pub root: PathBuf,
}

impl Vault {
    pub fn new(id: i64, name: impl Into<String>, root: PathBuf) -> Self {
        Self {
            id,
            name: name.into(),
            root,
        }
    }

    /// First-boot filesystem init: create the directory tree and seed
    /// SCHEMA.md / index.md / log.md. Idempotent — existing files are NEVER
    /// overwritten (a re-run after human edits must be a no-op).
    pub async fn init_fs(&self) -> Result<()> {
        for dir in DIRECTORIES {
            let path = self.root.join(dir);
            tokio::fs::create_dir_all(&path)
                .await
                .map_err(|e| CoreError::Io { path, source: e })?;
        }
        write_if_absent(&self.root.join("SCHEMA.md"), &schema::seed(&self.name)).await?;
        write_if_absent(&self.root.join("index.md"), &index::seed()).await?;
        write_if_absent(&self.root.join("log.md"), &log::seed(&self.name)).await?;
        Ok(())
    }
}

/// Write `content` to `path` only when the file does not exist yet.
async fn write_if_absent(path: &Path, content: &str) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    tokio::fs::write(path, content)
        .await
        .map_err(|e| CoreError::Io {
            path: path.to_path_buf(),
            source: e,
        })
}
