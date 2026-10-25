//! Atomic file mutation primitives — the ONLY place in `vault/` that touches
//! the filesystem with writes (VLET-02, PATTERNS §2.5: no MoriNet analog,
//! established fresh).
//!
//! Invariants:
//! - The temp file lives in the **same directory** as the target: a rename is
//!   only atomic within one filesystem (cross-device rename is a copy+unlink
//!   and can leave a half-written target).
//! - The temp name is dot-prefixed (`.page.md.tmp-<uuid>`) so Obsidian and
//!   glob scans ignore leftovers, and the uuid keeps concurrent temps from
//!   colliding.
//! - On ANY failure path the temp file is removed before the error returns;
//!   `rename` is the last step, so a crash mid-write can only leave a temp
//!   file, never a half-written page (ROADMAP SC2).

use std::path::Path;

use crate::error::{CoreError, Result};

/// Write `bytes` to `path` atomically: dot-prefixed temp in the target's own
/// directory, then `rename` onto the target. Parent directories are created
/// if missing (vault subdirs may not exist yet on first write).
pub async fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().ok_or_else(|| CoreError::Io {
        path: path.to_path_buf(),
        source: std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path has no parent directory",
        ),
    })?;
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|e| CoreError::Io {
            path: dir.to_path_buf(),
            source: e,
        })?;
    let file_name = path
        .file_name()
        .ok_or_else(|| CoreError::Io {
            path: path.to_path_buf(),
            source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no file name"),
        })?
        .to_string_lossy();
    let tmp = dir.join(format!(".{}.tmp-{}", file_name, uuid::Uuid::new_v4()));
    tokio::fs::write(&tmp, bytes)
        .await
        .map_err(|e| CoreError::Io {
            path: tmp.clone(),
            source: e,
        })?;
    if let Err(e) = tokio::fs::rename(&tmp, path).await {
        // Loud cleanup: never leave the temp behind on failure (PATTERNS §2.8).
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(CoreError::Io {
            path: path.to_path_buf(),
            source: e,
        });
    }
    Ok(())
}

/// Append `lines` to `path` as ONE complete atomic write: read-whole +
/// concat + `write_atomic`. Plain `fs::append` outside the writer lock is the
/// Pitfall 6 interleaving bug — a log entry must land whole or not at all.
pub async fn append_lines(path: &Path, lines: &[String]) -> Result<()> {
    let existing = match tokio::fs::read_to_string(path).await {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(CoreError::Io {
                path: path.to_path_buf(),
                source: e,
            })
        }
    };
    let mut content = existing;
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    for line in lines {
        content.push_str(line);
        content.push('\n');
    }
    write_atomic(path, content.as_bytes()).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn count_temps(dir: &Path) -> usize {
        std::fs::read_dir(dir)
            .expect("read dir")
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .count()
    }

    #[tokio::test]
    async fn write_atomic_creates_file_and_leaves_no_temps() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("nested/page.md");
        write_atomic(&target, b"hello").await.unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "hello");
        assert_eq!(count_temps(&tmp.path().join("nested")), 0);
    }

    #[tokio::test]
    async fn write_atomic_replaces_existing_content() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("page.md");
        write_atomic(&target, b"first").await.unwrap();
        write_atomic(&target, b"second").await.unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "second");
        assert_eq!(count_temps(tmp.path()), 0);
    }

    #[tokio::test]
    async fn write_atomic_cleans_temp_on_rename_failure() {
        let tmp = TempDir::new().unwrap();
        // A non-empty DIRECTORY at the target makes rename fail on POSIX
        // (ENOTEMPTY) — the forced-failure path must delete the temp.
        let target = tmp.path().join("page.md");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("inner"), "x").unwrap();
        let err = write_atomic(&target, b"boom").await.unwrap_err();
        assert!(matches!(err, CoreError::Io { .. }), "{err:?}");
        assert_eq!(count_temps(tmp.path()), 0, "temp must be cleaned up");
    }

    #[tokio::test]
    async fn append_lines_produces_one_contiguous_entry() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("log.md");
        write_atomic(&target, b"# Log\n").await.unwrap();
        append_lines(
            &target,
            &[
                "## [2026-10-25] create | Page".to_string(),
                "- wrote page.md".to_string(),
            ],
        )
        .await
        .unwrap();
        append_lines(&target, &["## [2026-10-25] update | Page".to_string()])
            .await
            .unwrap();
        let content = std::fs::read_to_string(&target).unwrap();
        assert_eq!(
            content,
            "# Log\n## [2026-10-25] create | Page\n- wrote page.md\n## [2026-10-25] update | Page\n"
        );
        assert_eq!(count_temps(tmp.path()), 0);
    }

    #[tokio::test]
    async fn append_lines_creates_missing_file() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("fresh.md");
        append_lines(&target, &["line".to_string()]).await.unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "line\n");
    }
}
