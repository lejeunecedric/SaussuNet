//! `reindex` — the D-01 rebuildability proof: the vault filesystem is the
//! sole source of truth and `page_registry` is a disposable projection.
//! Deleting `data/saussunet.db` (or `DELETE FROM page_registry`) costs zero
//! content: this scan rebuilds the registry from scratch (clear +
//! repopulate), exactly as ARCHITECTURE Pattern 3 / Key Data Flow 4 promise.
//!
//! Scope discipline: reindex performs MINIMAL validation only (title and
//! `type` present) — full taxonomy validation stays with the writer, the
//! sole mutation path (VLET-02). A malformed page is a lint-style warning
//! (`skipped <path>: <reason>` on stderr), never a dead command (T-04-02).

use std::path::{Path, PathBuf};

use crate::error::{CoreError, Result};
use crate::store::Store;
use crate::vault::{frontmatter, Vault, PAGE_DIRS};

/// Rebuild `page_registry` for `vault` from its filesystem contents:
/// `clear_pages(vault.id)` (scoped DELETE — only this vault's rows,
/// T-04-03/T-04-05) then one `upsert_page` per parseable page. Every store
/// call carries `vault.id` (VLET-04). Returns the number of pages indexed;
/// unparseable/incomplete pages are skipped with a stderr line and excluded
/// from the count (T-04-02).
pub async fn reindex(vault: &Vault, store: &Store) -> Result<u64> {
    let root = vault.root.canonicalize().map_err(|e| CoreError::Io {
        path: vault.root.clone(),
        source: e,
    })?;

    let mut files: Vec<(String, PathBuf)> = Vec::new();
    for dir in PAGE_DIRS {
        collect_markdown(&root, &root.join(dir), &mut files)?;
    }

    store.clear_pages(vault.id).await?;

    let mut count: u64 = 0;
    for (rel, path) in files {
        let raw = match tokio::fs::read_to_string(&path).await {
            Ok(raw) => raw,
            Err(e) => {
                eprintln!("skipped {rel}: {e}");
                continue;
            }
        };
        let (yaml, _body) = frontmatter::split(&raw);
        let fm = match frontmatter::parse(yaml) {
            Ok(fm) => fm,
            Err(e) => {
                // T-04-02: one bad file must not abort the whole reindex.
                eprintln!("skipped {rel}: {e}");
                continue;
            }
        };
        // Minimal validation only — full taxonomy checks belong to the writer.
        if fm.title.trim().is_empty() {
            eprintln!("skipped {rel}: title must be non-empty");
            continue;
        }
        if fm.page_type.trim().is_empty() {
            eprintln!("skipped {rel}: type must be present");
            continue;
        }
        let tags_json = serde_json::to_string(&fm.tags)
            .map_err(|e| CoreError::Frontmatter(format!("cannot encode tags: {e}")))?;
        store
            .upsert_page(
                vault.id,
                &rel,
                &fm.title,
                &fm.page_type,
                &tags_json,
                (!fm.updated.is_empty()).then_some(fm.updated.as_str()),
                // The projection carries the D-08 human-ownership signal:
                // a gen_hash map present on disk means writer-generated.
                fm.gen_hash.is_some(),
            )
            .await?;
        count += 1;
    }
    Ok(count)
}

/// Recursively collect `.md` files under `dir` (relative to `root`).
/// Synchronous walk (plain std::fs — a personal-scale scan; async
/// recursion would need boxing for no benefit). Dot-prefixed entries are
/// skipped (matches the writer's `.{name}.tmp-*` temp convention —
/// in-flight temps must never be indexed). Each file is canonicalized and
/// checked for containment in the canonical root BEFORE it is queued: a
/// symlink planted inside the vault must not pull external files into the
/// registry (T-04-01).
fn collect_markdown(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        // A vault that has never used one of the four directories simply has
        // no pages there — not an error.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(CoreError::Io {
                path: dir.to_path_buf(),
                source: e,
            })
        }
    };
    for entry in entries {
        let entry = entry.map_err(|e| CoreError::Io {
            path: dir.to_path_buf(),
            source: e,
        })?;
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        let path = entry.path();
        let file_type = entry.file_type().map_err(|e| CoreError::Io {
            path: path.clone(),
            source: e,
        })?;
        if file_type.is_dir() {
            collect_markdown(root, &path, out)?;
            continue;
        }
        // `file_type` is lstat-based: a symlink is neither file nor dir and
        // is skipped outright; the canonicalize check below is the second,
        // defense-in-depth layer for paths reached the normal way.
        if !file_type.is_file() || path.extension().is_none_or(|e| e != "md") {
            continue;
        }
        // T-04-01: resolve symlinks, then require containment in the vault.
        let canonical = match path.canonicalize() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("skipped {}: {e}", rel_for(root, &path));
                continue;
            }
        };
        if !canonical.starts_with(root) {
            eprintln!(
                "skipped {}: escapes the vault root (symlink?)",
                rel_for(root, &path)
            );
            continue;
        }
        out.push((rel_for(root, &path), canonical));
    }
    Ok(())
}

/// Vault-relative path with forward slashes (registry `path` column format).
fn rel_for(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}
