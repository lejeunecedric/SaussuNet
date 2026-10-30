//! Deterministic write-path validator (plan 03-06, PIPE-06 / D-11).
//!
//! Nothing an LLM composed reaches the vault unless this module vouches for
//! it first: link whitelist (index.md ∪ entity registry), frontmatter schema,
//! and provenance presence/validity are checked BEFORE any disk write. Pure
//! Rust — no LLM call, no network, no heuristics (Pitfall 3) — and strictly
//! read-only: the only disk mutation stays `writer::write_page` (VLET-02).
//!
//! The self-citation guard (Pitfall 4) is enforced here deterministically,
//! not by prompt: a `sources` entry inside the wiki layer is rejected, and
//! every `^[...]` provenance marker must target a real file under `raw/`.

use std::collections::HashSet;
use std::path::Path;

use crate::error::{CoreError, Result};
use crate::store::Store;

use super::{frontmatter, schema, Vault};

/// Wiki-layer directories (PAGE_DIRS shape): a `sources` entry whose first
/// segment is one of these is the LLM citing its own output (Pitfall 4).
const WIKI_DIRS: [&str; 4] = ["entities", "concepts", "comparisons", "queries"];

/// Whitelist of legal outbound wikilink targets (D-11/PIPE-06): stems parsed
/// from `index.md` entries (`- [[stem]] — title`, written by
/// [`super::index::upsert_entry`]) ∪ entity canonical names ∪ registry
/// `page_path` stems. Built deterministically at validation time.
pub async fn link_whitelist(vault: &Vault, store: &Store) -> Result<HashSet<String>> {
    let mut set = HashSet::new();

    let index_path = vault.root().join("index.md");
    if let Ok(content) = tokio::fs::read_to_string(&index_path).await {
        for line in content.lines() {
            if let Some(rest) = line.trim_start().strip_prefix("- [[") {
                if let Some(end) = rest.find("]]") {
                    let stem = rest[..end].trim();
                    if !stem.is_empty() {
                        set.insert(stem.to_string());
                    }
                }
            }
        }
    }

    for entity in store.list_entities(vault.id).await? {
        set.insert(entity.canonical_name.clone());
        if let Some(page_path) = &entity.page_path {
            if let Some(stem) = Path::new(page_path).file_stem() {
                set.insert(stem.to_string_lossy().to_string());
            }
        }
    }

    Ok(set)
}

/// Distinct `[[target]]` wikilink targets in `body`, in first-seen order.
/// Same stripping contract as [`frontmatter::count_outbound_wikilinks`]:
/// `[[target|alias]]` and `[[target#heading]]` count as `target`;
/// `![[image.png]]` embeds are not outbound page links.
pub fn extract_wikilinks(body: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(open) = rest.find("[[") {
        let is_embed = open > 0 && rest.as_bytes()[open - 1] == b'!';
        let after = &rest[open + 2..];
        let Some(close) = after.find("]]") else {
            break;
        };
        if !is_embed {
            let target = after[..close].split(['|', '#']).next().unwrap_or("").trim();
            if !target.is_empty() && seen.insert(target.to_string()) {
                out.push(target.to_string());
            }
        }
        rest = &after[close + 2..];
    }
    out
}

/// Distinct `^[...]` provenance marker targets in `body` (superscript-caret
/// form, D-07), in first-seen order. Deliberately broader than the
/// `^[raw/...]` regex: markers pointing OUTSIDE `raw/` must be visible to
/// the validator so they can be REJECTED, not silently ignored.
pub fn extract_provenance(body: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(open) = rest.find("^[") {
        let after = &rest[open + 2..];
        let Some(close) = after.find(']') else { break };
        let target = after[..close].trim();
        if !target.is_empty() && seen.insert(target.to_string()) {
            out.push(target.to_string());
        }
        rest = &after[close + 1..];
    }
    out
}

/// True iff `rel` (already checked to start with `raw/`) is a traversal-free
/// relative path whose file exists under the vault root. Read-only check.
async fn raw_target_exists(vault: &Vault, rel: &str) -> bool {
    if rel.starts_with('/') || rel.split('/').any(|seg| seg.is_empty() || seg == "..") {
        return false;
    }
    tokio::fs::try_exists(vault.root().join(rel))
        .await
        .unwrap_or(false)
}

/// The PIPE-06 gate: validate one LLM-generated page BEFORE any disk write.
///
/// Checks (all violations collected into ONE [`CoreError::Validation`] so
/// the pipeline logs a fully actionable rejection):
/// 1. frontmatter parses and passes [`frontmatter::validate_page`] (schema +
///    SCHEMA.md taxonomy) — same gate `write_page` applies, folded into the
///    uniform `Validation` error so one bad page never fails the whole job;
/// 2. `sources` non-empty; every entry starts with `raw/`, is traversal-free,
///    and the file exists under `raw/`; any entry whose first segment is a
///    wiki-layer directory is rejected outright (Pitfall 4 self-citation);
/// 3. body contains ≥1 `^[...]` provenance marker and every marker target
///    starts with `raw/` and exists on disk (D-11);
/// 4. every outbound wikilink target is in [`link_whitelist`] or equals the
///    page's own stem.
///
/// Never writes anything: this is a pure function over the page plus
/// read-only FS reads (index.md, raw/ existence) and a registry query.
pub async fn validate_generated_page(
    vault: &Vault,
    store: &Store,
    rel_path: &str,
    content: &str,
) -> Result<()> {
    let mut violations: Vec<String> = Vec::new();

    // 1. Frontmatter schema gate (reuse the llm-wiki contract).
    let (yaml, body) = frontmatter::split(content);
    match frontmatter::parse(yaml).and_then(|fm| {
        let taxonomy = schema::parse_tag_taxonomy(vault)?;
        frontmatter::validate_page(&fm, &taxonomy)?;
        Ok(fm)
    }) {
        Ok(fm) => {
            // 2. Provenance: frontmatter sources.
            if fm.sources.is_empty() {
                violations
                    .push("sources: empty — generated pages must cite raw/ evidence".to_string());
            }
            for src in &fm.sources {
                let first = src.split('/').next().unwrap_or("");
                if WIKI_DIRS.contains(&first) {
                    violations.push(format!(
                        "sources: {src:?} cites the wiki layer — self-citation rejected (Pitfall 4)"
                    ));
                } else if !src.starts_with("raw/") {
                    violations.push(format!(
                        "sources: {src:?} is not a raw/ path — generated pages cite raw/ evidence only"
                    ));
                } else if !raw_target_exists(vault, src).await {
                    violations.push(format!("sources: {src:?} does not exist under raw/"));
                }
            }
        }
        Err(e) => violations.push(format!("frontmatter: {e}")),
    }

    // 3. Provenance markers in the body.
    let markers = extract_provenance(body);
    if markers.is_empty() {
        violations.push("provenance: body has no ^[raw/...] marker".to_string());
    }
    for marker in &markers {
        if !marker.starts_with("raw/") {
            violations.push(format!(
                "provenance: ^[{marker}] targets outside raw/ — provenance must point at raw/ evidence"
            ));
        } else if !raw_target_exists(vault, marker).await {
            violations.push(format!(
                "provenance: ^[{marker}] target does not exist under raw/"
            ));
        }
    }

    // 4. Link whitelist (D-11): index.md stems ∪ registry ∪ own stem.
    let own_stem = Path::new(rel_path)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| rel_path.to_string());
    let whitelist = link_whitelist(vault, store).await?;
    for link in extract_wikilinks(body) {
        if link != own_stem && !whitelist.contains(&link) {
            violations.push(format!(
                "wikilink: [[{link}]] is not in index.md or the entity registry"
            ));
        }
    }

    if !violations.is_empty() {
        return Err(CoreError::Validation(violations.join("; ")));
    }
    Ok(())
}
