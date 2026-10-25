//! THE sole vault mutation entry point (VLET-02). Every page creation or
//! modification — CLI now, generators in Phase 3+ — goes through
//! [`write_page`]: per-vault write lock, path guard, frontmatter validation,
//! wikilink floor, atomic temp+rename, index/log maintenance, and the
//! `page_registry` projection. No other code path may create or modify vault
//! files; nothing in `vault/` writes outside `atomic::write_atomic`.

use std::path::{Path, PathBuf};

use chrono::Utc;

use crate::error::{CoreError, Result};
use crate::store::Store;

use super::{atomic, frontmatter, index, log, schema, Vault};

/// Outcome of a successful [`write_page`] call. Conflict variants
/// (gen_hash mismatch, protected page — D-06/D-07) arrive in Plan 01-03;
/// the enum absorbs them without changing the signature.
#[derive(Debug, Clone)]
pub enum WriteOutcome {
    Written { path: PathBuf },
}

/// Sanitize a title/path before it lands in log.md / index.md: newlines and
/// `## [` sequences must not let a crafted title forge log entries (T-02-05).
/// Newlines become spaces (a title can no longer start a new line), and the
/// `## [` sequence is collapsed to `##[` so whole-file scanners cannot match
/// a forged entry shape inside a subject.
fn sanitize_for_nav(s: &str) -> String {
    s.replace(['\r', '\n'], " ").replace("## [", "##[")
}

/// Map a page type to its index.md section heading.
fn index_section(page_type: &str) -> &'static str {
    match page_type {
        "entity" => "Entities",
        "concept" => "Concepts",
        "comparison" => "Comparisons",
        "query" => "Queries",
        other => unreachable!("validate_page rejects types outside PAGE_TYPES: {other}"),
    }
}

/// Write (create or update) a wiki page. Sole vault mutation entry (VLET-02).
///
/// Flow — all inside the per-vault `write_lock` so page, index.md and log.md
/// mutate as one serialized unit (Pitfall 6):
///
/// 1. path guard (T-02-01)
/// 2. frontmatter parse + validate against the SCHEMA.md taxonomy
/// 3. at least 2 distinct outbound wikilinks (VLET-01)
/// 4. date normalization (create: created=updated=today; update: preserve
///    created, bump updated)
/// 5. atomic write
/// 6. registry projection (D-01)
/// 7. index.md entry + counters
/// 8. log.md entry (+ rotation)
pub async fn write_page(
    vault: &Vault,
    store: &Store,
    rel_path: &str,
    content: &str,
) -> Result<WriteOutcome> {
    let _guard = vault.write_lock.lock().await;

    // 1. Path guard (T-02-01): traversal, raw/, symlink rejection.
    let target = vault.resolve_page_path(rel_path).await?;

    // Read-before-validate: existence decides create vs update semantics.
    let existing = tokio::fs::read_to_string(&target).await.ok();

    // 2. Frontmatter: split (never truncates), parse, validate vs taxonomy.
    let (yaml, body) = frontmatter::split(content);
    let mut fm = frontmatter::parse(yaml)?;
    let taxonomy = schema::parse_tag_taxonomy(vault)?;
    frontmatter::validate_page(&fm, &taxonomy)?;

    // 3. Wikilink floor (VLET-01 / llm-wiki convention).
    if frontmatter::count_outbound_wikilinks(body) < 2 {
        return Err(CoreError::Frontmatter(
            "page requires at least 2 outbound [[wikilinks]]".to_string(),
        ));
    }

    // 4. Date normalization: create sets both to today; update PRESERVES
    // the created date from the existing file (not the incoming input) and
    // bumps updated.
    let today = Utc::now().format("%Y-%m-%d").to_string();
    let is_update = existing.is_some();
    fm.updated = today.clone();
    fm.created = match &existing {
        Some(prev) => {
            let (prev_yaml, _) = frontmatter::split(prev);
            frontmatter::parse(prev_yaml)
                .ok()
                .filter(|prev_fm| !prev_fm.created.trim().is_empty())
                .map(|prev_fm| prev_fm.created)
                .unwrap_or_else(|| today.clone())
        }
        None => today.clone(),
    };

    // 5. Atomic write (VLET-02): temp+rename in the target's directory.
    let page = frontmatter::serialize(&fm, body);
    atomic::write_atomic(&target, page.as_bytes()).await?;

    // 6. Projection row — metadata only, never the body (D-01).
    let tags_json = serde_json::to_string(&fm.tags)
        .map_err(|e| CoreError::Frontmatter(format!("cannot serialize tags: {e}")))?;
    store
        .upsert_page(
            vault.id,
            rel_path,
            &fm.title,
            &fm.page_type,
            &tags_json,
            Some(&fm.updated),
            fm.gen_hash.is_some(),
        )
        .await?;

    // 7. index.md: entry under the type section + header counters.
    let stem = Path::new(rel_path)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| rel_path.to_string());
    index::upsert_entry(
        vault,
        index_section(&fm.page_type),
        &stem,
        &sanitize_for_nav(&fm.title),
    )
    .await?;

    // 8. log.md: one complete entry (rotation checked inside).
    let action = if is_update { "update" } else { "create" };
    log::append_entry(
        vault,
        &format!("## [{today}] {action} | {}", sanitize_for_nav(&fm.title)),
        &[format!("- wrote {rel_path}")],
    )
    .await?;

    Ok(WriteOutcome::Written { path: target })
}
