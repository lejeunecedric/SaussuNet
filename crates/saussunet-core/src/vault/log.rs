//! log.md seed + append-only maintenance, following the llm-wiki skill
//! "log.md Template" (append-only, `## [YYYY-MM-DD] action | subject`,
//! rotation past 500 entries to `log-YYYY.md`).

use chrono::Utc;

use crate::error::{CoreError, Result};

use super::{atomic, Vault};

/// llm-wiki rotation rule: rotate when the log exceeds 500 entries.
pub const ROTATION_THRESHOLD: usize = 500;

/// Build the initial log.md with the creation entry for domain `domain`.
pub fn seed(domain: &str) -> String {
    let today = Utc::now().format("%Y-%m-%d");
    format!(
        "# Wiki Log

> Chronological record of all wiki actions. Append-only.
> Format: `## [YYYY-MM-DD] action | subject`
> Actions: ingest, update, query, lint, create, archive, delete
> When this file exceeds 500 entries, rotate: rename to log-YYYY.md, start fresh.

## [{today}] create | Wiki initialized
- Domain: {domain}
- Structure created with SCHEMA.md, index.md, log.md
"
    )
}

/// Count `## [` entry lines in a log.md body.
pub fn entry_count(content: &str) -> usize {
    content.lines().filter(|l| l.starts_with("## [")).count()
}

/// True when `content` exceeds the 500-entry rotation threshold.
pub fn should_rotate(content: &str) -> bool {
    entry_count(content) > ROTATION_THRESHOLD
}

/// The header block of a log.md (everything before the first `## [` entry),
/// used to start a fresh log after rotation.
pub fn fresh_header(content: &str) -> String {
    let mut header = String::new();
    for line in content.lines() {
        if line.starts_with("## [") {
            break;
        }
        header.push_str(line);
        header.push('\n');
    }
    header
}

/// Append one complete entry (`header` line + `lines`) to log.md as a
/// single atomic write under the caller's vault write lock — plain appends
/// outside the lock are the Pitfall 6 interleaving bug. Rotates first when
/// the result would exceed 500 entries (llm-wiki rule): the old log moves
/// to `log-<YYYY>.md` and a fresh log.md keeps only the standard header.
pub async fn append_entry(vault: &Vault, header: &str, lines: &[String]) -> Result<()> {
    let path = vault.root().join("log.md");
    let existing = match tokio::fs::read_to_string(&path).await {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(CoreError::Io {
                path: path.clone(),
                source: e,
            })
        }
    };

    let mut entry = String::new();
    entry.push_str(header);
    entry.push('\n');
    for line in lines {
        entry.push_str(line);
        entry.push('\n');
    }

    if should_rotate(&existing) {
        // Archive the OLD log (the new entry starts the fresh file).
        let year = Utc::now().format("%Y");
        let rotated = vault.root().join(format!("log-{year}.md"));
        let archive_prefix = tokio::fs::read_to_string(&rotated)
            .await
            .unwrap_or_default();
        let mut archived = archive_prefix;
        archived.push_str(&existing);
        atomic::write_atomic(&rotated, archived.as_bytes()).await?;
        let mut fresh = fresh_header(&existing);
        if !fresh.ends_with('\n') {
            fresh.push('\n');
        }
        fresh.push_str(&entry);
        atomic::write_atomic(&path, fresh.as_bytes()).await?;
        return Ok(());
    }

    let mut content = existing;
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(&entry);
    atomic::write_atomic(&path, content.as_bytes()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a log.md body with `n` entries (plus the standard header).
    fn fixture_with_entries(n: usize) -> String {
        let mut s = String::from(
            "# Wiki Log\n\n> Chronological record of all wiki actions. Append-only.\n",
        );
        for i in 0..n {
            s.push_str(&format!(
                "## [2026-10-01] update | Page {i}\n- touched page-{i}.md\n"
            ));
        }
        s
    }

    #[test]
    fn seed_has_header_and_first_entry() {
        let s = seed("main");
        assert!(s.contains("# Wiki Log"));
        assert!(s.contains("## ["));
        assert!(s.contains("Wiki initialized"));
        assert!(s.contains("- Domain: main"));
        assert!(s.contains("rotate"));
    }

    #[test]
    fn rotation_triggers_only_above_500_entries() {
        assert!(!should_rotate(&fixture_with_entries(500)));
        assert!(should_rotate(&fixture_with_entries(501)));
    }

    #[test]
    fn fresh_header_keeps_header_and_drops_entries() {
        let fixture = fixture_with_entries(501);
        let fresh = fresh_header(&fixture);
        assert!(fresh.contains("# Wiki Log"));
        assert!(fresh.contains("Append-only"));
        assert!(!fresh.contains("## ["));
    }

    #[tokio::test]
    async fn append_entry_rotates_oversized_log() {
        let tmp = tempfile::TempDir::new().unwrap();
        let vault = Vault::new(1, "main", tmp.path().to_path_buf());
        let log_path = tmp.path().join("log.md");
        atomic::write_atomic(&log_path, fixture_with_entries(501).as_bytes())
            .await
            .unwrap();
        append_entry(
            &vault,
            "## [2026-10-25] create | Fresh",
            &["- wrote fresh.md".to_string()],
        )
        .await
        .unwrap();
        let year = chrono::Utc::now().format("%Y").to_string();
        let rotated = tmp.path().join(format!("log-{year}.md"));
        assert!(rotated.exists(), "log-{year}.md must exist after rotation");
        assert_eq!(
            entry_count(&std::fs::read_to_string(&rotated).unwrap()),
            501
        );
        let fresh = std::fs::read_to_string(&log_path).unwrap();
        assert!(fresh.contains("# Wiki Log"), "fresh log keeps the header");
        assert_eq!(entry_count(&fresh), 1);
        assert!(fresh.contains("create | Fresh"));
    }
}
