//! `raw/` writer — the ONLY sanctioned writer for the immutable evidence
//! layer (Pitfall 4: `writer::write_page` deliberately refuses `raw/`).
//!
//! Same discipline as the page writer: hold `vault.write_lock` for the whole
//! critical section (VLET-02 single writer) and land bytes exclusively via
//! `atomic::write_atomic` (Pitfall 6: the only disk-write primitive).
//! Naming follows D-07: `raw/[subdir/]YYYY-MM-DD-slug.ext`.

use std::path::PathBuf;

use chrono::Utc;

use crate::error::{CoreError, Result};

use super::atomic;
use super::Vault;

/// Frontmatter block for a raw record (D-07): source_url + ingested + sha256.
#[derive(Debug, Clone)]
pub struct RawFrontmatter {
    pub source_url: String,
    /// RFC3339 timestamp of ingestion.
    pub ingested: String,
    /// sha256 hex of the stored content.
    pub sha256: String,
}

/// Write one raw evidence record (D-07): `raw/[subdir/]YYYY-MM-DD-slug.ext`
/// with `source_url` / `ingested` / `sha256` frontmatter, under the vault
/// single-writer lock and via `atomic::write_atomic` ONLY (Pitfalls 4 + 6).
/// A same-day re-ingest of the same slug atomically overwrites (allowed);
/// nothing may ever escape `raw/`.
pub async fn write_raw(
    vault: &Vault,
    subdir: Option<&str>,
    slug: &str,
    ext: &str,
    frontmatter: &RawFrontmatter,
    body: &[u8],
) -> Result<PathBuf> {
    let slug = sanitize_component(slug)?;
    let ext = sanitize_component(ext)?;
    let mut rel = PathBuf::from("raw");
    if let Some(sub) = subdir {
        // Subdirectories are structural (articles/papers/transcripts/assets):
        // any traversal character is a hard refusal, not a silent fold.
        if sub.is_empty() || sub.contains(['.', '/', '\\']) {
            return Err(CoreError::Vault(format!(
                "raw subdir {sub:?} must be a plain name (no '.' or path separators)"
            )));
        }
        rel.push(sanitize_component(sub)?);
    }
    let date = Utc::now().format("%Y-%m-%d");
    rel.push(format!("{date}-{slug}.{ext}"));
    let target = vault.root.join(&rel);
    // Belt-and-braces containment: sanitisation already forbids escapes, but
    // assert it (T-02-01 shape) before touching disk.
    if !target.starts_with(vault.root.join("raw")) {
        return Err(CoreError::Vault(format!(
            "raw path escapes raw/: {}",
            target.display()
        )));
    }

    // VLET-02: same single-writer lock as page writes — raw appends and page
    // writes never interleave.
    let _guard = vault.write_lock.lock().await;
    let mut bytes = format!(
        "---\nsource_url: {}\ningested: {}\nsha256: {}\n---\n\n",
        frontmatter.source_url, frontmatter.ingested, frontmatter.sha256
    )
    .into_bytes();
    bytes.extend_from_slice(body);
    atomic::write_atomic(&target, &bytes).await?;
    Ok(target)
}

/// ASCII-fold one path component to `[a-z0-9-]`: every non-alphanumeric
/// byte becomes `-`, runs collapse, leading/trailing dashes are trimmed.
/// An empty result (e.g. `"../.."`) is refused — never a silent empty name.
fn sanitize_component(raw: &str) -> Result<String> {
    let folded = raw
        .to_ascii_lowercase()
        .chars()
        .fold(String::new(), |mut acc, c| {
            acc.push(if c.is_ascii_alphanumeric() { c } else { '-' });
            acc
        });
    let mut out = String::new();
    let mut prev_dash = true; // suppress leading dashes
    for c in folded.chars() {
        if c == '-' {
            if !prev_dash {
                out.push('-');
            }
            prev_dash = true;
        } else {
            out.push(c);
            prev_dash = false;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        return Err(CoreError::Vault(format!(
            "path component {raw:?} is empty after sanitisation"
        )));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fm() -> RawFrontmatter {
        RawFrontmatter {
            source_url: "https://example.com/post".to_string(),
            ingested: "2026-10-30T12:00:00+00:00".to_string(),
            sha256: "abc123".to_string(),
        }
    }

    #[tokio::test]
    async fn write_raw_lands_dated_file_with_frontmatter() {
        let tmp = tempfile::TempDir::new().unwrap();
        let vault = Vault::new(1, "main", tmp.path().join("vault"));
        vault.init_fs().await.unwrap();

        let path = write_raw(&vault, None, "Test Slug!", "md", &fm(), b"body bytes")
            .await
            .expect("write_raw");

        let today = Utc::now().format("%Y-%m-%d").to_string();
        let expected = vault.root.join("raw").join(format!("{today}-test-slug.md"));
        assert_eq!(path, expected, "D-07 naming: raw/YYYY-MM-DD-slug.ext");

        let content = tokio::fs::read_to_string(&expected).await.unwrap();
        assert!(content.contains("source_url: https://example.com/post"));
        assert!(content.contains("ingested: 2026-10-30T12:00:00+00:00"));
        assert!(content.contains("sha256: abc123"));
        assert!(
            content.ends_with("body bytes"),
            "exact body bytes: {content}"
        );

        // Same slug + same day: atomic overwrite, no corruption, no dup file.
        let path2 = write_raw(&vault, None, "Test Slug!", "md", &fm(), b"v2")
            .await
            .expect("same-day re-ingest overwrite");
        assert_eq!(path2, expected);
        let after = tokio::fs::read(&expected).await.unwrap();
        assert!(after.ends_with(b"v2"), "overwrite must replace the body");
        assert_eq!(after.windows(7).filter(|w| w == b"sha256:").count(), 1);
    }

    #[tokio::test]
    async fn write_raw_refuses_escapes_and_lands_in_subdir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let vault = Vault::new(1, "main", tmp.path().join("vault"));
        vault.init_fs().await.unwrap();

        // traversal attempts are neutralised (sanitised, never escape raw/)
        let err = write_raw(&vault, None, "../..", "md", &fm(), b"x").await;
        assert!(
            err.is_err(),
            "empty-after-sanitisation slug must be refused"
        );

        let path = write_raw(&vault, Some("articles"), "ok", "md", &fm(), b"x")
            .await
            .expect("subdir write");
        assert!(path.starts_with(vault.root.join("raw")));
        assert!(path.starts_with(vault.root.join("raw").join("articles")));

        let err = write_raw(&vault, Some("../entities"), "ok", "md", &fm(), b"x")
            .await
            .expect_err("subdir traversal must not escape raw/");
        assert!(matches!(err, CoreError::Vault(_)));
    }

    #[test]
    fn slug_sanitisation() {
        assert_eq!(sanitize_component("Test Slug!").unwrap(), "test-slug");
        assert_eq!(sanitize_component("a/b\\c..d").unwrap(), "a-b-c-d");
        // non-ASCII is dropped (ascii-fold), never smuggled into a path
        assert_eq!(sanitize_component("Ünïcödé — dash").unwrap(), "n-c-d-dash");
        assert!(sanitize_component("../..").is_err());
        assert!(sanitize_component("").is_err());
    }
}
