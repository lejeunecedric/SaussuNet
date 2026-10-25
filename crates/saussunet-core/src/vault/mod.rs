//! Vault layer: `Vault` (FS boundary keyed by vault id, D-04) + first-boot
//! FS initialization + the single-writer lock and path guard (VLET-02).
//!
//! Every vault mutation goes through `writer::write_page`, which holds
//! `write_lock` for its whole critical section; `resolve_page_path` is the
//! T-02-01 traversal guard shared by all writes.

pub mod atomic;
pub mod frontmatter;
pub mod index;
pub mod log;
pub mod schema;
pub mod writer;

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

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
///
/// NOT `Clone` on purpose: `write_lock` is the per-vault single-writer lock
/// (VLET-02, Pitfall 6) and cloning a `Mutex` would silently mint a second,
/// independent lock. Share one instance (e.g. via `Arc`).
#[derive(Debug)]
pub struct Vault {
    pub id: i64,
    pub name: String,
    pub root: PathBuf,
    /// Single per-vault write lock (VLET-02): serializes ALL mutations —
    /// page, index.md, log.md — so log appends can never interleave.
    pub write_lock: tokio::sync::Mutex<()>,
}

impl Vault {
    pub fn new(id: i64, name: impl Into<String>, root: PathBuf) -> Self {
        Self {
            id,
            name: name.into(),
            root,
            write_lock: tokio::sync::Mutex::new(()),
        }
    }

    /// Canonical vault root (set after boot; may be the configured path when
    /// constructed directly in tests).
    pub fn root(&self) -> &Path {
        &self.root
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

    /// T-02-01 path guard: turn an operator-supplied relative path into a
    /// canonical path guaranteed to live inside the vault root.
    ///
    /// Guard order (MoriNet WebDAV lesson: canonicalize BEFORE trusting):
    /// 1. reject empty, absolute, any `..` segment, and any path under `raw/`
    ///    (immutable evidence layer — page writes never touch it, Pitfall 4);
    /// 2. join to root, canonicalize the deepest existing ancestor, and
    ///    assert prefix containment against the canonical root;
    /// 3. reject a target that exists as a symlink (it could point outside).
    pub async fn resolve_page_path(&self, rel: &str) -> Result<PathBuf> {
        if rel.trim().is_empty() {
            return Err(CoreError::Vault("path must be non-empty".to_string()));
        }
        let rel_path = Path::new(rel);
        if rel_path.is_absolute() {
            return Err(CoreError::Vault(format!(
                "path must be relative to the vault root, got {rel:?}"
            )));
        }
        let mut first = true;
        for comp in rel_path.components() {
            match comp {
                Component::ParentDir => {
                    return Err(CoreError::Vault(format!(
                        "path must not contain '..' segments, got {rel:?}"
                    )))
                }
                Component::Normal(name) if first && name == "raw" => {
                    return Err(CoreError::Vault(
                        "raw/ is the immutable evidence layer; page writes are refused (Pitfall 4)"
                            .to_string(),
                    ))
                }
                Component::Normal(_) => {}
                other => {
                    return Err(CoreError::Vault(format!(
                        "path must be a plain relative path, got {rel:?} (unexpected {other:?})"
                    )))
                }
            }
            first = false;
        }

        let joined = self.root.join(rel_path);
        // Symlink check on the UNRESOLVED path: canonicalize below would
        // follow the link and report a generic escape instead.
        if let Ok(meta) = tokio::fs::symlink_metadata(&joined).await {
            if meta.file_type().is_symlink() {
                return Err(CoreError::Vault(format!(
                    "target is a symlink, refusing to write through it: {rel:?}"
                )));
            }
        }
        let canonical_root = canonicalize_ancestor(&self.root).ok_or_else(|| CoreError::Io {
            path: self.root.clone(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "vault root does not exist"),
        })?;
        let canonical = canonicalize_ancestor(&joined).ok_or_else(|| {
            CoreError::Vault(format!("cannot resolve path {rel:?} against vault root"))
        })?;
        if !canonical.starts_with(&canonical_root) {
            return Err(CoreError::Vault(format!(
                "path escapes the vault root: {rel:?} resolves to {canonical:?}"
            )));
        }
        Ok(canonical)
    }
}

/// Canonicalize the deepest existing ancestor of `path`, re-appending the
/// missing tail. Returns `None` only when no ancestor exists at all.
fn canonicalize_ancestor(path: &Path) -> Option<PathBuf> {
    let mut tail: Vec<OsString> = Vec::new();
    let mut cur = path.to_path_buf();
    loop {
        if let Ok(real) = cur.canonicalize() {
            let mut out = real;
            for name in tail.iter().rev() {
                out.push(name);
            }
            return Some(out);
        }
        let parent = cur.parent().map(Path::to_path_buf);
        match (cur.file_name(), parent) {
            (Some(name), Some(parent)) => {
                tail.push(name.to_os_string());
                cur = parent;
            }
            _ => return None,
        }
    }
}

/// Write `content` to `path` only when the file does not exist yet.
/// Init-only helper (boot seeding, before any writer traffic); all writes
/// still go through `atomic::write_atomic` so the no-direct-write invariant
/// holds across `vault/` (VLET-02 grep audit).
async fn write_if_absent(path: &Path, content: &str) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    atomic::write_atomic(path, content.as_bytes()).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn vault_in_tmp(tmp: &TempDir) -> Vault {
        let vault = Vault::new(1, "main", tmp.path().to_path_buf());
        vault.init_fs().await.unwrap();
        vault
    }

    #[tokio::test]
    async fn accepts_plain_page_paths() {
        let tmp = TempDir::new().unwrap();
        let vault = vault_in_tmp(&tmp).await;
        let resolved = vault.resolve_page_path("concepts/foo.md").await.unwrap();
        assert!(resolved.starts_with(vault.root.canonicalize().unwrap()));
        assert!(resolved.ends_with("concepts/foo.md"));
    }

    #[tokio::test]
    async fn rejects_parent_traversal() {
        let tmp = TempDir::new().unwrap();
        let vault = vault_in_tmp(&tmp).await;
        let err = vault
            .resolve_page_path("../evil.md")
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("'..'"), "{err}");
    }

    #[tokio::test]
    async fn rejects_absolute_paths() {
        let tmp = TempDir::new().unwrap();
        let vault = vault_in_tmp(&tmp).await;
        let err = vault
            .resolve_page_path("/tmp/evil.md")
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("relative"), "{err}");
    }

    #[tokio::test]
    async fn rejects_raw_writes() {
        let tmp = TempDir::new().unwrap();
        let vault = vault_in_tmp(&tmp).await;
        let err = vault
            .resolve_page_path("raw/articles/x.md")
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("raw/"), "{err}");
    }

    #[tokio::test]
    async fn rejects_empty_path() {
        let tmp = TempDir::new().unwrap();
        let vault = vault_in_tmp(&tmp).await;
        assert!(vault.resolve_page_path("  ").await.is_err());
    }

    #[tokio::test]
    async fn rejects_symlink_escape() {
        let tmp = TempDir::new().unwrap();
        let vault = vault_in_tmp(&tmp).await;
        // A page that is a symlink pointing outside the vault root.
        let outside = tmp.path().join("..").join("outside-secret.md");
        atomic::write_atomic(&outside, b"secret").await.unwrap();
        std::os::unix::fs::symlink(outside.canonicalize().unwrap(), tmp.path().join("spy.md"))
            .unwrap();
        let err = vault
            .resolve_page_path("spy.md")
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("symlink"), "{err}");
    }

    #[tokio::test]
    async fn rejects_symlinked_directory_escape() {
        let tmp = TempDir::new().unwrap();
        let vault = vault_in_tmp(&tmp).await;
        let outside_dir = TempDir::new().unwrap();
        std::os::unix::fs::symlink(
            outside_dir.path().canonicalize().unwrap(),
            tmp.path().join("escape"),
        )
        .unwrap();
        // canonicalize resolves the symlinked dir → prefix check must fire.
        let err = vault
            .resolve_page_path("escape/x.md")
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("escapes the vault root"), "{err}");
    }
}
