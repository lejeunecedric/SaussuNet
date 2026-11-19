//! index.md seed + per-write maintenance, following the llm-wiki skill
//! "index.md Template" (sectioned by type, alphabetical entries, header
//! counters `Last updated | Total pages`), reworked to the OKF §8 shape
//! (D-17/D-18): root frontmatter `okf_version: "0.2"` and entries
//! `- [Title](/path.md) — description`.

use chrono::Utc;

use crate::error::{CoreError, Result};

use super::{atomic, Vault};

/// Build the initial index.md (empty vault: `Total pages: 0`), prefixed by
/// the D-17 frontmatter block — the ONE place frontmatter is permitted in an
/// index, and it carries ONLY `okf_version: "0.2"` (quoted).
pub fn seed() -> String {
    let today = Utc::now().format("%Y-%m-%d");
    format!(
        "---
okf_version: \"0.2\"
---
# Wiki Index

> Content catalog. Every wiki page listed under its type with a one-line summary.
> Read this first to find relevant pages for any query.
> Last updated: {today} | Total pages: 0

## Entities
<!-- Alphabetical within section -->

## Concepts

## Comparisons

## Queries
"
    )
}

/// Extract the bundle-relative path from a `- [Title](/path.md) — desc`
/// entry line (None for anything else). Shared shape contract with
/// `validate::index_entry_map` — the paired Pitfall-1 coupling.
fn entry_path(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("- [")?;
    let close = rest.find("](")?;
    let after = &rest[close + 2..];
    let end = after.find(')')?;
    Some(&after[..end])
}

/// Insert or replace `- [Title](/path.md) — description` alphabetically
/// within the type section (dedup keyed by the entry path), then rewrite the
/// header line `> Last updated: YYYY-MM-DD | Total pages: N` (N = count of
/// new-format entry lines across sections — llm-wiki skill: counters +
/// alphabetical). The page path is derived from the section directory
/// (`Concepts` → `/concepts/<stem>.md`). Atomic via `atomic::write_atomic`;
/// must be called under the vault write lock (writer does this).
pub async fn upsert_entry(
    vault: &Vault,
    section: &str,
    stem: &str,
    title: &str,
    description: Option<&str>,
) -> Result<()> {
    let path = vault.root().join("index.md");
    let content = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| CoreError::Io {
            path: path.clone(),
            source: e,
        })?;
    let lines: Vec<&str> = content.lines().collect();
    let sec_header = format!("## {section}");
    let start = lines
        .iter()
        .position(|l| l.trim_end() == sec_header)
        .ok_or_else(|| {
            CoreError::Vault(format!(
                "index.md has no '{sec_header}' section — fix the index structure"
            ))
        })?;
    let end = lines[start + 1..]
        .iter()
        .position(|l| l.starts_with("## "))
        .map(|p| start + 1 + p)
        .unwrap_or(lines.len());

    let page_path = format!("/{}/{}.md", section.to_lowercase(), stem);
    let new_line = match description.map(str::trim).filter(|d| !d.is_empty()) {
        Some(desc) => format!("- [{title}]({page_path}) — {desc}"),
        None => format!("- [{title}]({page_path})"),
    };
    // Section entries, minus any existing line for this path (D-18 dedup).
    let mut entries: Vec<String> = lines[start + 1..end]
        .iter()
        .filter(|l| l.starts_with("- ["))
        .filter(|l| entry_path(l) != Some(page_path.as_str()))
        .map(|l| l.to_string())
        .collect();
    entries.push(new_line.clone());
    entries.sort();

    // Rebuild: header + pre-section lines, section header, non-entry lines
    // (e.g. the alphabetical comment), sorted entries, then the rest.
    let mut out: Vec<String> = Vec::with_capacity(lines.len() + 1);
    out.extend(lines[..=start].iter().map(|l| l.to_string()));
    for l in &lines[start + 1..end] {
        if !l.starts_with("- [") {
            out.push(l.to_string());
        }
    }
    out.extend(entries);
    out.extend(lines[end..].iter().map(|l| l.to_string()));

    // Header counters: N = every `- [Title](/path.md)` entry line across
    // all sections (same commit as the format flip — Pitfall 1).
    let total = out
        .iter()
        .filter(|l| l.starts_with("- [") && l.contains("]("))
        .count();
    let today = Utc::now().format("%Y-%m-%d");
    let header = format!("> Last updated: {today} | Total pages: {total}");
    let mut header_done = false;
    for l in out.iter_mut() {
        if l.starts_with("> Last updated:") {
            *l = header.clone();
            header_done = true;
            break;
        }
    }
    if !header_done {
        return Err(CoreError::Vault(
            "index.md lost its '> Last updated:' header line — fix the index structure".to_string(),
        ));
    }

    let mut text = out.join("\n");
    if !text.ends_with('\n') {
        text.push('\n');
    }
    atomic::write_atomic(&path, text.as_bytes()).await
}

#[cfg(test)]
mod tests {
    use super::seed;

    #[test]
    fn seed_has_header_and_sections() {
        let s = seed();
        assert!(s.starts_with("---\nokf_version: \"0.2\"\n---\n"));
        assert!(s.contains("# Wiki Index"));
        assert!(s.contains("Total pages: 0"));
        for section in ["## Entities", "## Concepts", "## Comparisons", "## Queries"] {
            assert!(s.contains(section));
        }
    }
}
