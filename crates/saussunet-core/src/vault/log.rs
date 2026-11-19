//! log.md seed + maintenance, reworked to the OKF §9 shape (D-19): entries
//! group under `## YYYY-MM-DD` headings, newest first, as bullets
//! `* **<action>**: <subject>` with optional indented sub-bullets
//! (`  - <detail>`). Rotation past 500 bullets to `log-YYYY.md` unchanged.

use chrono::Utc;

use crate::error::{CoreError, Result};

use super::writer::sanitize_for_nav;
use super::{atomic, Vault};

/// llm-wiki rotation rule: rotate when the log exceeds 500 entries.
pub const ROTATION_THRESHOLD: usize = 500;

/// True when `line` is a `## YYYY-MM-DD` date-group heading (single shape
/// implementation: [`super::okf::log_date_ok`], okflint LOG_HEADING+DATE_OK).
fn is_date_heading(line: &str) -> bool {
    line.strip_prefix("## ")
        .is_some_and(|rest| super::okf::log_date_ok(rest.trim()))
}

/// Build the initial log.md with the creation entry for domain `domain`
/// (D-19: no frontmatter, today's date group, bullet entry).
pub fn seed(domain: &str) -> String {
    let today = Utc::now().format("%Y-%m-%d");
    format!(
        "# Wiki Log

> Chronological record of all wiki actions, grouped by day, newest first.
> Format: `## YYYY-MM-DD` groups with `* **action**: subject` bullets
> Actions: ingest, update, query, lint, create, archive, delete
> When this file exceeds 500 entries, rotate: rename to log-YYYY.md, start fresh.

## {today}
* **create**: Wiki initialized
  - Domain: {domain}
  - Structure created with SCHEMA.md, index.md, log.md
"
    )
}

/// Count `* ` main-bullet entry lines in a log.md body (Pitfall 3: the
/// counter must follow the entry format or rotation never fires; indented
/// `  - ` detail sub-bullets are not entries).
pub fn entry_count(content: &str) -> usize {
    content.lines().filter(|l| l.starts_with("* ")).count()
}

/// True when `content` exceeds the 500-entry rotation threshold.
pub fn should_rotate(content: &str) -> bool {
    entry_count(content) > ROTATION_THRESHOLD
}

/// The header block of a log.md (everything before the first `## YYYY-MM-DD`
/// date-group heading), used to start a fresh log after rotation.
pub fn fresh_header(content: &str) -> String {
    let mut header = String::new();
    for line in content.lines() {
        if is_date_heading(line) {
            break;
        }
        header.push_str(line);
        header.push('\n');
    }
    header
}

/// Append one entry (`* **{action}**: {subject}` bullet, each non-empty
/// `details` line rendered as an indented sub-bullet `  - {detail}`) to
/// log.md, inserted at the TOP of today's `## YYYY-MM-DD` group — creating
/// the group directly below the header when absent (D-19, newest-first).
/// Subject and details pass through `sanitize_for_nav` so a crafted title
/// cannot forge a date heading or entry shape (Pitfall 8). Single atomic
/// write under the caller's vault write lock — plain appends outside the
/// lock are the Pitfall 6 interleaving bug. Rotates first when the result
/// would exceed 500 entries (llm-wiki rule): the old log moves to
/// `log-<YYYY>.md` and a fresh log.md keeps only the standard header.
pub async fn append_entry(
    vault: &Vault,
    action: &str,
    subject: &str,
    details: &[String],
) -> Result<()> {
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

    let mut entry = format!("* **{action}**: {}\n", sanitize_for_nav(subject));
    for detail in details {
        let detail = detail.trim();
        if !detail.is_empty() {
            entry.push_str(&format!("  - {}\n", sanitize_for_nav(detail)));
        }
    }

    let mut content = existing;
    if should_rotate(&content) {
        // Archive the OLD log (the new entry starts the fresh file).
        let year = Utc::now().format("%Y");
        let rotated = vault.root().join(format!("log-{year}.md"));
        let archive_prefix = tokio::fs::read_to_string(&rotated)
            .await
            .unwrap_or_default();
        let mut archived = archive_prefix;
        archived.push_str(&content);
        atomic::write_atomic(&rotated, archived.as_bytes()).await?;
        content = fresh_header(&content);
    }

    // Insert at the top of today's group; create the group when absent.
    let today_heading = format!("## {}", Utc::now().format("%Y-%m-%d"));
    let lines: Vec<&str> = content.lines().collect();
    let header_end = lines
        .iter()
        .position(|l| is_date_heading(l))
        .unwrap_or(lines.len());
    let mut out: Vec<String> = lines[..header_end].iter().map(|l| l.to_string()).collect();
    if out.last().map(|l| !l.trim().is_empty()).unwrap_or(false) {
        out.push(String::new());
    }
    let entry_lines: Vec<String> = entry.lines().map(|l| l.to_string()).collect();
    let today_pos = lines[header_end..]
        .iter()
        .position(|l| l.trim_end() == today_heading)
        .map(|p| header_end + p);
    match today_pos {
        Some(p) => {
            // `out` already holds `lines[..header_end]` — extend only with
            // the region between the header and today's heading, or the
            // header block would be re-emitted on every append (H1).
            out.extend(lines[header_end..=p].iter().map(|l| l.to_string()));
            out.extend(entry_lines.clone());
            out.extend(lines[p + 1..].iter().map(|l| l.to_string()));
        }
        None => {
            out.push(today_heading);
            out.extend(entry_lines.clone());
            if header_end < lines.len() {
                out.push(String::new());
            }
            out.extend(lines[header_end..].iter().map(|l| l.to_string()));
        }
    }

    let mut text = out.join("\n");
    if !text.ends_with('\n') {
        text.push('\n');
    }
    atomic::write_atomic(&path, text.as_bytes()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a log.md body with `n` bullet entries under one date group
    /// (plus the standard header).
    fn fixture_with_entries(n: usize) -> String {
        let mut s = String::from(
            "# Wiki Log\n\n> Chronological record of all wiki actions.\n\n## 2026-10-01\n",
        );
        for i in 0..n {
            s.push_str(&format!(
                "* **update**: Page {i}\n  - touched page-{i}.md\n"
            ));
        }
        s
    }

    #[test]
    fn seed_has_header_and_first_entry() {
        let s = seed("main");
        assert!(!s.starts_with("---"), "log.md carries no frontmatter");
        assert!(s.contains("# Wiki Log"));
        assert!(s.contains("* **create**: Wiki initialized"));
        assert!(s.contains("  - Domain: main"));
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
        assert!(fresh.contains("Chronological"));
        assert!(!fresh.contains("## 2026-10-01"));
    }

    #[tokio::test]
    async fn append_entry_rotates_oversized_log() {
        let tmp = tempfile::TempDir::new().unwrap();
        let vault = Vault::new(1, "main", tmp.path().to_path_buf());
        let log_path = tmp.path().join("log.md");
        atomic::write_atomic(&log_path, fixture_with_entries(501).as_bytes())
            .await
            .unwrap();
        append_entry(&vault, "create", "Fresh", &["wrote fresh.md".to_string()])
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
        assert!(fresh.contains("* **create**: Fresh"));
    }
}
