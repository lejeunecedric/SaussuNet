//! Threshold gate + registry resolve/upsert + page assembly + validator
//! funnel (plans 03-03/03-06, D-10/D-14..D-17, PIPE-06).
//!
//! The ONLY disk write in the extraction pipeline is
//! [`vault::writer::write_generated_page`] → `write_page` (validator funnel,
//! PIPE-06): the LLM's output becomes page markdown here and nothing else —
//! the deterministic validator rejects dangling links, missing provenance,
//! and wiki-as-source before any write, and every page still passes the
//! writer's frontmatter/wikilink/gen_hash gauntlet.

use std::path::PathBuf;
use std::sync::Arc;

use chrono::Utc;

use crate::error::{CoreError, Result};
use crate::llm::{generate_json_typed, LlmProvider};
use crate::store::Store;
use crate::vault::frontmatter::PageFrontmatter;
use crate::vault::writer::WriteOutcome;
use crate::vault::Vault;
use crate::vault::{self, frontmatter, schema};

use super::prompts::{pass2_prompt, ExistingPage};
use super::schemas::{CandidateKind, ExtractCandidates, PageContent};

/// Deterministic slug for page paths: lowercase, every run of
/// non-alphanumerics collapses to a single `-`, edges trimmed.
pub(crate) fn slugify(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_dash = true; // trim a leading separator
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.extend(c.to_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

fn kind_str(kind: CandidateKind) -> &'static str {
    match kind {
        CandidateKind::Entity => "entity",
        CandidateKind::Concept => "concept",
    }
}

fn dir_for(kind: CandidateKind) -> &'static str {
    match kind {
        CandidateKind::Entity => "entities",
        CandidateKind::Concept => "concepts",
    }
}

/// Truncate for debug logging — prompts/outputs may carry user secrets,
/// so they are never logged whole (Phase 2 D-03 pattern).
fn clip(s: &str) -> String {
    const MAX: usize = 400;
    if s.chars().count() <= MAX {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(MAX).collect();
        t.push_str("…[truncated]");
        t
    }
}

/// Threshold-gated page writes for one source (D-09 pass 2 side).
///
/// For every pass-1 candidate: resolve-before-create (D-15), alias merge
/// (D-16), source-set recording for EVERY candidate incl. sub-threshold
/// (D-17), then — iff `central || distinct_sources >= min_sources`
/// (D-10/D-14) — a pass-2 call generating/merging the page, assembled
/// through the frontmatter model and written via `write_page`.
/// Returns the paths actually written.
#[allow(clippy::too_many_arguments)]
pub async fn gate_and_write(
    store: &Store,
    vault: &Arc<Vault>,
    provider: &Arc<dyn LlmProvider>,
    min_sources: u32,
    raw_rel: &str,
    source_id: &str,
    markdown: &str,
    pass1: &ExtractCandidates,
) -> Result<Vec<PathBuf>> {
    let taxonomy = schema::parse_tag_taxonomy(vault)?;
    let mut written = Vec::new();
    let mut rejected: u32 = 0;

    for candidate in &pass1.candidates {
        if candidate.name.trim().is_empty() {
            continue;
        }
        // (a) resolve-before-create (D-15) + alias accumulation (D-16).
        let resolved = store.resolve_entity(vault.id, &candidate.name).await?;
        let (canonical, page_path_hint) = match &resolved {
            Some(rec) => {
                store
                    .merge_aliases(vault.id, &rec.canonical_name, &candidate.aliases)
                    .await?;
                (rec.canonical_name.clone(), rec.page_path.clone())
            }
            None => {
                let aliases = serde_json::to_string(&candidate.aliases)
                    .map_err(|e| CoreError::Vault(format!("aliases serialize: {e}")))?;
                store
                    .upsert_entity(
                        vault.id,
                        &candidate.name,
                        &aliases,
                        None,
                        Some(kind_str(candidate.kind)),
                    )
                    .await?;
                (candidate.name.clone(), None)
            }
        };

        // Source set recorded for EVERY candidate (D-17) so accumulation
        // across future sources can cross the threshold later.
        store
            .add_entity_source(vault.id, &canonical, source_id)
            .await?;

        // (b) threshold gate (D-10/D-14/D-17): passing mentions below the
        // bar never write a page (PIPE-03).
        let count = store.count_entity_sources(vault.id, &canonical).await?;
        if !candidate.central && count < min_sources {
            tracing::debug!(
                "pipeline: {canonical} sub-threshold ({count}/{min_sources}, not central) — no page"
            );
            continue;
        }

        // (c) pass 2: generate/merge seeing existing page + new evidence.
        let rel_path = page_path_hint
            .unwrap_or_else(|| format!("{}/{}.md", dir_for(candidate.kind), slugify(&canonical)));
        let existing = tokio::fs::read_to_string(vault.root.join(&rel_path))
            .await
            .unwrap_or_default();
        let page_ctx = ExistingPage {
            rel_path: rel_path.clone(),
            content: existing.clone(),
        };
        let prompt = pass2_prompt(&page_ctx, raw_rel, markdown, candidate);
        tracing::debug!("pipeline pass-2 prompt ({canonical}): {}", clip(&prompt));
        let content: PageContent =
            generate_json_typed(provider.as_ref(), &prompt, &PageContent::json_schema_hint())
                .await?;
        tracing::debug!(
            "pipeline pass-2 output ({canonical}): {}",
            clip(&serde_json::to_string(&content).unwrap_or_default())
        );

        let page = assemble_page(
            &content,
            &canonical,
            candidate.kind,
            raw_rel,
            &existing,
            &taxonomy,
        );
        let outcome = match vault::writer::write_generated_page(vault, store, &rel_path, &page)
            .await
        {
            Ok(outcome) => outcome,
            // PIPE-06: deterministic validator rejection. Log rule names +
            // offending targets only (never the body — a poisoned page may
            // carry attacker text, D-03), record the rejected content for
            // the Phase 5 review queue, and CONTINUE: one bad page never
            // fails the whole job.
            Err(CoreError::Validation(reason)) => {
                rejected += 1;
                tracing::warn!("pipeline: {rel_path} rejected by write-path validator: {reason}");
                store
                    .insert_proposal(vault.id, &rel_path, "validation_rejected", &page)
                    .await?;
                continue;
            }
            Err(e) => return Err(e),
        };
        match outcome {
            WriteOutcome::Written { path } => {
                // Record the page path on the registry row, preserving the
                // (possibly alias-merged) aliases from the previous write.
                let aliases_json = store
                    .resolve_entity(vault.id, &canonical)
                    .await?
                    .map(|r| r.aliases_json)
                    .unwrap_or_else(|| "[]".to_string());
                store
                    .upsert_entity(
                        vault.id,
                        &canonical,
                        &aliases_json,
                        Some(&rel_path),
                        Some(kind_str(candidate.kind)),
                    )
                    .await?;
                written.push(path);
            }
            // gen_hash protection wins (Phase 1 D-06/D-08): log + continue,
            // never fight the three-way merge.
            WriteOutcome::ConflictRecorded {
                reason,
                proposal_id,
            } => {
                tracing::warn!(
                    "pipeline: write to {rel_path} skipped (conflict: {reason}, proposal #{proposal_id})"
                );
            }
            WriteOutcome::Refused { reason } => {
                tracing::warn!("pipeline: write to {rel_path} refused ({reason})");
            }
        }
    }
    tracing::info!(
        "pipeline: {source_id} — {} page(s) written, {rejected} validation-rejected",
        written.len()
    );
    Ok(written)
}

/// Assemble the full page markdown: llm-wiki frontmatter (sources merged
/// with the existing page's list) + `## Summary` + sections, every claim
/// line keeping its `^[raw/...]` marker (D-11). `write_page` re-serializes
/// the frontmatter itself — this only has to parse and validate.
fn assemble_page(
    content: &PageContent,
    canonical: &str,
    kind: CandidateKind,
    raw_rel: &str,
    existing_md: &str,
    taxonomy: &[String],
) -> String {
    let today = Utc::now().format("%Y-%m-%d").to_string();

    // Tags: keep only taxonomy members (SCHEMA.md is the format authority);
    // fall back to a safe Meta tag when the model produced none usable.
    let mut tags: Vec<String> = content
        .tags
        .iter()
        .filter(|t| taxonomy.iter().any(|x| x == *t))
        .cloned()
        .collect();
    if tags.is_empty() {
        tags.push("reference".to_string());
    }

    // Page type: trust the model only within the taxonomy of page types.
    let page_type = if frontmatter::PAGE_TYPES.contains(&content.page_type.as_str()) {
        content.page_type.clone()
    } else {
        kind_str(kind).to_string()
    };

    // Sources: existing page's list first, then this raw/ path (dedup).
    let mut sources: Vec<String> = Vec::new();
    if !existing_md.trim().is_empty() {
        let (yaml, _) = frontmatter::split(existing_md);
        if let Ok(fm) = frontmatter::parse(yaml) {
            sources = fm.sources;
        }
    }
    if !sources.iter().any(|s| s == raw_rel) {
        sources.push(raw_rel.to_string());
    }

    let mut body = String::new();
    if !content.summary.trim().is_empty() {
        body.push_str("## Summary\n\n");
        body.push_str(content.summary.trim());
        body.push_str("\n\n");
    }
    for section in &content.sections {
        if !section.heading.trim().is_empty() {
            body.push_str(&format!("## {}\n\n", section.heading.trim()));
        }
        body.push_str(section.markdown.trim());
        body.push_str("\n\n");
    }

    let fm = PageFrontmatter {
        title: if content.title.trim().is_empty() {
            canonical.to_string()
        } else {
            content.title.clone()
        },
        created: today.clone(),
        updated: today,
        page_type,
        tags,
        sources,
        confidence: None,
        contested: if content.contested { Some(true) } else { None },
        contradictions: if content.contradictions.is_empty() {
            None
        } else {
            Some(content.contradictions.clone())
        },
        protected: None,
        gen_hash: None,
        other: Default::default(),
    };
    frontmatter::serialize(&fm, &body)
}
