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

use super::{atomic, frontmatter, ghash, index, log, schema, Vault};

/// Outcome of a [`write_page`] call. The three-way merge (D-05..D-08)
/// decides between them: `Written` (first write or clean overwrite),
/// `ConflictRecorded` (human edit detected — write skipped, proposal
/// recorded, D-06) and `Refused` (protected page D-07 or human-owned page
/// D-08 — proposal still recorded, never written).
#[derive(Debug, Clone)]
pub enum WriteOutcome {
    Written { path: PathBuf },
    ConflictRecorded { reason: String, proposal_id: i64 },
    Refused { reason: String },
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

/// True for paths inside the wiki layer the D-08 human-owned rule covers:
/// first path segment `entities` or `concepts`.
fn in_wiki_layer(rel_path: &str) -> bool {
    matches!(
        Path::new(rel_path).components().next(),
        Some(std::path::Component::Normal(seg)) if seg == "entities" || seg == "concepts"
    )
}

/// Write (create or update) a wiki page. Sole vault mutation entry (VLET-02).
///
/// Flow — all inside the per-vault `write_lock` so page, index.md and log.md
/// mutate as one serialized unit (Pitfall 6):
///
/// 1. path guard (T-02-01)
/// 2. frontmatter parse + validate against the SCHEMA.md taxonomy
/// 3. at least 2 distinct outbound wikilinks (VLET-01)
/// 4. three-way merge check (D-05..D-08): protected → refuse; gen_hash
///    mismatch → skip write + record conflict + log; no gen_hash in the
///    wiki layer → human-owned refuse; otherwise proceed
/// 5. date normalization (create: created=updated=today; update: preserve
///    created, bump updated)
/// 6. gen_hash map computed over the final content and embedded (D-05,
///    recorded from the FIRST write — Pitfall 7)
/// 7. atomic write
/// 8. registry projection (D-01)
/// 9. index.md entry + counters
/// 10. log.md entry (+ rotation)
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
    // Review W-4: ONLY NotFound means "no file yet". Any other read failure
    // (EACCES, EIO, EISDIR…) is propagated and the write refused — swallowing
    // it would treat an unreadable protected/human-owned page as a first
    // write and overwrite it (rename needs only directory permission).
    let existing = match tokio::fs::read_to_string(&target).await {
        Ok(s) => Some(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            return Err(CoreError::Io {
                path: target.clone(),
                source: e,
            })
        }
    };

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

    let today = Utc::now().format("%Y-%m-%d").to_string();
    let is_update = existing.is_some();
    let stem = Path::new(rel_path)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| rel_path.to_string());

    // 4. Three-way merge check (D-05..D-08) — BEFORE any mutation, still
    // under the write lock. Compare the on-disk file against the gen_hash
    // map the writer recorded from its own previous write.
    if let Some(prev) = &existing {
        let (prev_yaml, prev_body) = frontmatter::split(prev);
        // Unparseable frontmatter parses to None: agent-authorship cannot be
        // PROVEN, which is exactly the D-08 human-owned condition.
        let prev_fm = frontmatter::parse(prev_yaml).ok();
        let existing_title = prev_fm
            .as_ref()
            .map(|f| f.title.clone())
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| stem.clone());
        let protected = prev_fm.as_ref().and_then(|f| f.protected).unwrap_or(false);
        let stored_map = prev_fm.as_ref().and_then(|f| f.gen_hash.clone());

        if protected {
            // D-07: protected pages refuse ALL writer mutations; the
            // proposal is still recorded for the Phase 5 review queue.
            let id = store
                .insert_proposal(vault.id, rel_path, "protected", content)
                .await?;
            log::append_entry(
                vault,
                &format!(
                    "## [{today}] refused | {} (protected page)",
                    sanitize_for_nav(&existing_title)
                ),
                &[
                    format!("- refused write to {rel_path} (protected page)"),
                    format!("- proposal recorded (pending_proposals #{id})"),
                ],
            )
            .await?;
            return Ok(WriteOutcome::Refused {
                reason: "protected".to_string(),
            });
        }

        match stored_map {
            Some(stored) => {
                // Recompute the map over the on-disk file exactly the way
                // the write side computed it (gen_hash stripped from the
                // frontmatter hash on BOTH sides — T-03-04).
                let current = ghash::compute_map(prev_yaml, prev_body);
                if ghash::human_edited(&stored, &current) {
                    // D-06: human edit proven — SKIP the write, record the
                    // proposal, log the conflict. Never a silent overwrite.
                    let id = store
                        .insert_proposal(vault.id, rel_path, "gen_hash_mismatch", content)
                        .await?;
                    log::append_entry(
                        vault,
                        &format!(
                            "## [{today}] conflict | {}",
                            sanitize_for_nav(&existing_title)
                        ),
                        &[
                            format!("- skipped write to {rel_path} (human edit detected)"),
                            format!("- proposal recorded (pending_proposals #{id})"),
                        ],
                    )
                    .await?;
                    return Ok(WriteOutcome::ConflictRecorded {
                        reason: "gen_hash_mismatch".to_string(),
                        proposal_id: id,
                    });
                }
                // gen_hash present and matching: agent-only edits → clean
                // overwrite below.
            }
            None => {
                // D-08: no gen_hash map we wrote (missing, or frontmatter
                // unparseable/absent). Inside the wiki layer that proves
                // nothing about agent authorship → human-owned, refuse.
                // Outside entities//concepts/ (queries/, comparisons/) the
                // rule is scoped away: agent-writable, overwrite below.
                if in_wiki_layer(rel_path) {
                    let id = store
                        .insert_proposal(vault.id, rel_path, "human_owned", content)
                        .await?;
                    log::append_entry(
                        vault,
                        &format!(
                            "## [{today}] refused | {} (human-owned page)",
                            sanitize_for_nav(&existing_title)
                        ),
                        &[
                            format!("- refused write to {rel_path} (human-owned page)"),
                            format!("- proposal recorded (pending_proposals #{id})"),
                        ],
                    )
                    .await?;
                    return Ok(WriteOutcome::Refused {
                        reason: "human_owned".to_string(),
                    });
                }
            }
        }
    }

    // 5. Date normalization: create sets both to today; update PRESERVES
    // the created date from the existing file (not the incoming input) and
    // bumps updated.
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

    // 6. gen_hash provenance (D-05 / Pitfall 7): strip any incoming map,
    // compute the map over the FINAL content (dates normalized), and embed
    // it — recorded from the FIRST write, so agent authorship is provable
    // from day one. The draft is serialized without the map so the
    // frontmatter hash excludes the map itself (T-03-04), and the section
    // hashes cover the exact bytes that will land on disk.
    fm.gen_hash = None;
    let draft = frontmatter::serialize(&fm, body);
    let (draft_yaml, draft_body) = frontmatter::split(&draft);
    fm.gen_hash = Some(ghash::compute_map(draft_yaml, draft_body));
    let page = frontmatter::serialize(&fm, body);

    // 7. Atomic write (VLET-02): temp+rename in the target's directory.
    atomic::write_atomic(&target, page.as_bytes()).await?;

    // 8. Projection row — metadata only, never the body (D-01).
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

    // 9. index.md: entry under the type section + header counters.
    index::upsert_entry(
        vault,
        index_section(&fm.page_type),
        &stem,
        &sanitize_for_nav(&fm.title),
    )
    .await?;

    // 10. log.md: one complete entry (rotation checked inside).
    let action = if is_update { "update" } else { "create" };
    log::append_entry(
        vault,
        &format!("## [{today}] {action} | {}", sanitize_for_nav(&fm.title)),
        &[format!("- wrote {rel_path}")],
    )
    .await?;

    Ok(WriteOutcome::Written { path: target })
}
