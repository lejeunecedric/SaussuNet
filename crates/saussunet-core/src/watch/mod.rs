//! Continuous source watching (Phase 4, SRCC-01/SRCC-02).
//!
//! Pure logic first: `multistatus` parses RFC 4918 multistatus / RFC 6578
//! sync-report bodies, `diff` classifies walk-vs-state deltas. 04-04 adds
//! `pub mod dav;` (HTTP client) and `poll_once` wiring here; 04-05 completes
//! the walk: depth-capped BFS, complete-walk-only removals, the sha256 gate
//! and last_error bookkeeping.

pub mod dav;
pub mod diff;
pub mod multistatus;

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

use chrono::{SecondsFormat, Utc};

use crate::config::ResolvedWatch;
use crate::error::{CoreError, Result};
use crate::pipeline;
use crate::store::Store;
use crate::vault::{self, Vault};

use dav::DavClient;
use diff::{diff_walk, StateEntry, WalkEntry, WatchDelta};

/// One poll of one source: depth-capped BFS walk → diff vs watch_state →
/// download + enqueue deltas → removals (complete walks only). Returns the
/// count of enqueued jobs. Errors are returned, never panicked, and recorded
/// as `sync_state.last_error` (sanitized); the 04-06 interval loop owns WARN
/// logging (D-11: no secrets, no response bodies in logs). `poll_once`
/// itself never logs at warn/error level — DEBUG only.
///
/// Crash-safety invariants (Pitfall 2): a per-file `watch_state` update only
/// AFTER `insert_job` returned; removals only after a FULLY completed walk
/// (a partial walk applies additions/changes but prunes nothing — Pitfall 3).
pub async fn poll_once(
    store: &Store,
    vault: &Arc<Vault>,
    src: &ResolvedWatch,
    max_fetch_bytes: u64,
) -> Result<usize> {
    // Sub-second precision so consecutive polls strictly advance
    // `last_poll_at` (second precision could tie within one tick).
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::AutoSi, true);
    let outcome = poll_inner(store, vault, src, max_fetch_bytes, &now).await;
    match &outcome {
        Ok(_) => {
            store
                .record_sync_poll(vault.id, &src.source_id, Some(&now), None)
                .await?;
        }
        Err(e) => {
            // DavClient/Fetch errors are already sanitized to
            // scheme+host+path + reason (D-02 hygiene); strip newlines so a
            // server-influenced message can never forge log/state lines.
            let msg = sanitize_line(&e.to_string());
            store
                .record_sync_poll(vault.id, &src.source_id, Some(&now), Some(&msg))
                .await?;
        }
    }
    outcome
}

/// The walk/diff/apply body. Returns Err on a partial walk (after applying
/// the partial set's additions/changes) so the caller records `last_error`.
async fn poll_inner(
    store: &Store,
    vault: &Arc<Vault>,
    src: &ResolvedWatch,
    max_fetch_bytes: u64,
    now: &str,
) -> Result<usize> {
    let client = DavClient::new(src)?;

    // --- BFS walk (D-04, SRCC-01): one Depth:1 PROPFIND per collection,
    // root at depth 0, stop descending once depth + 1 > depth_cap (cap 4 ⇒
    // PROPFINDs at depths 0..4). `propfind_children` is the ONLY request
    // primitive and it always sends Depth: 1 — never Depth: infinity.
    let mut walk: Vec<WalkEntry> = Vec::new();
    let mut walk_err: Option<String> = None;
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();
    queue.push_back((String::new(), 0usize));
    // Defensive visited-set on collection rel paths (cyclic hrefs would loop).
    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(String::new());
    while let Some((rel, depth)) = queue.pop_front() {
        match client.propfind_children(&rel).await {
            Ok(entries) => {
                // status-404 entries (sync removals) are ignored — sync lands 04-07.
                for e in entries.iter().filter(|e| e.status == 200) {
                    // Parser paths are relative to the walked collection.
                    let path = if rel.is_empty() {
                        e.path.clone()
                    } else {
                        format!("{rel}/{}", e.path)
                    };
                    if e.is_collection
                        && depth < src.depth_cap as usize
                        && visited.insert(path.clone())
                    {
                        queue.push_back((path.clone(), depth + 1));
                    }
                    walk.push(WalkEntry {
                        path,
                        etag: e.etag.clone(),
                        size: e.size,
                        is_dir: e.is_collection,
                    });
                }
            }
            Err(e) => {
                // Keep everything collected so far — a partial walk still
                // flows its additions/changes; only removals are gated.
                tracing::debug!("watch walk: PROPFIND {rel:?} failed: {e}");
                if walk_err.is_none() {
                    walk_err = Some(e.to_string());
                }
            }
        }
    }
    let walk_complete = walk_err.is_none();

    let rows = store.list_watch_rows(vault.id, &src.source_id).await?;
    let state: Vec<StateEntry> = rows
        .iter()
        .map(|r| StateEntry {
            path: r.path.clone(),
            etag: r.etag.clone(),
            size: r.size,
            is_dir: r.is_dir,
            sha256: r.sha256.clone(),
        })
        .collect();
    let deltas = diff_walk(&walk, &state, &src.extensions);

    // Directories: state bookkeeping rows only (etag/size NULL), never
    // enqueued. diff_walk emits no delta for them by design.
    for w in walk.iter().filter(|w| w.is_dir) {
        store
            .upsert_watch_row(
                vault.id,
                &src.source_id,
                &w.path,
                true,
                None,
                None,
                None,
                now,
            )
            .await?;
    }

    let mut lines: Vec<String> = Vec::new();
    let mut enqueued = 0usize;
    for delta in deltas {
        match delta {
            delta @ (WatchDelta::Added(_) | WatchDelta::Changed(_)) => {
                let verb = if matches!(delta, WatchDelta::Added(_)) {
                    "added"
                } else {
                    "changed"
                };
                let entry = match delta {
                    WatchDelta::Added(w) | WatchDelta::Changed(w) => w,
                    _ => unreachable!("matched above"),
                };
                let bytes = client.get_bytes(&entry.path, max_fetch_bytes).await?;
                let sha = crate::pipeline::fetch::sha256_hex(&bytes);
                // D-07 sha256 backstop: identical bytes under a rotated
                // (weak) etag update the row only — no job, no log line.
                let prior_sha = state
                    .iter()
                    .find(|s| s.path == entry.path)
                    .and_then(|s| s.sha256.as_deref());
                if prior_sha == Some(sha.as_str()) {
                    store
                        .upsert_watch_row(
                            vault.id,
                            &src.source_id,
                            &entry.path,
                            false,
                            entry.etag.as_deref(),
                            entry.size,
                            Some(&sha),
                            now,
                        )
                        .await?;
                    continue;
                }
                let filename = entry
                    .path
                    .rsplit('/')
                    .next()
                    .unwrap_or(&entry.path)
                    .to_string();
                let ext = filename
                    .rsplit_once('.')
                    .map(|(_, e)| e.to_ascii_lowercase())
                    .unwrap_or_default();
                // D-08 routing: .md/.txt → ingest_raw, .pdf → ingest_pdf. diff_walk
                // already dropped anything outside the allowlist (Skipped), so the
                // else branch only fires for custom allowlist entries (no job).
                if ext == "md" || ext == "txt" {
                    let source_hint =
                        format!("{}/{}", src.url.as_str().trim_end_matches('/'), entry.path);
                    pipeline::submit_raw(
                        store,
                        vault,
                        &bytes,
                        &filename,
                        &source_hint,
                        max_fetch_bytes,
                    )
                    .await?;
                } else if ext == "pdf" {
                    pipeline::submit_pdf(store, vault, &bytes, &filename, max_fetch_bytes).await?;
                } else {
                    continue;
                }
                enqueued += 1;
                // crash-safety (Pitfall 2): durable job first, state second.
                // Worst case after a crash between the two = one duplicate
                // enqueue, absorbed by the worker's sha256 no-op.
                store
                    .upsert_watch_row(
                        vault.id,
                        &src.source_id,
                        &entry.path,
                        false,
                        entry.etag.as_deref(),
                        entry.size,
                        Some(&sha),
                        now,
                    )
                    .await?;
                lines.push(format!("- {verb}: {}", sanitize_line(&entry.path)));
            }
            WatchDelta::Skipped(w) => {
                // D-08 skip-note: log ONCE — the state row is the
                // "already noted" marker (upserted with sha256 NULL).
                if !state.iter().any(|s| s.path == w.path) {
                    lines.push(format!(
                        "- skipped (extension not allowlisted): {}",
                        sanitize_line(&w.path)
                    ));
                    store
                        .upsert_watch_row(
                            vault.id,
                            &src.source_id,
                            &w.path,
                            false,
                            w.etag.as_deref(),
                            w.size,
                            None,
                            now,
                        )
                        .await?;
                }
            }
            WatchDelta::Removed(s) => {
                // Pitfall 3 — THE gate: removals are unreachable unless EVERY
                // PROPFIND of this poll succeeded. One network blip can never
                // log the whole tree as removed or wipe watch_state.
                if !walk_complete {
                    continue;
                }
                store
                    .delete_watch_row(vault.id, &src.source_id, &s.path)
                    .await?;
                if !s.is_dir {
                    lines.push(format!("- removed: {}", sanitize_line(&s.path)));
                }
                // Removed dirs: row deleted silently (bookkeeping only).
            }
        }
    }

    // log.md discipline: ONE append_entry call per poll, only when something
    // happened — a no-op poll is completely silent (T9).
    if !lines.is_empty() {
        let header = format!("## [{}] update | Watch", Utc::now().format("%Y-%m-%d"));
        vault::log::append_entry(vault, &header, &lines).await?;
    }

    if let Some(msg) = walk_err {
        return Err(CoreError::Dav(msg));
    }
    Ok(enqueued)
}

/// Log/state-line injection guard: untrusted server paths (and error text
/// built from them) must never carry a newline into log.md or last_error.
fn sanitize_line(s: &str) -> String {
    s.replace(['\r', '\n'], " ")
}
