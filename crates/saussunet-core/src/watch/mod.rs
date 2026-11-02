//! Continuous source watching (Phase 4, SRCC-01/SRCC-02).
//!
//! Pure logic first: `multistatus` parses RFC 4918 multistatus / RFC 6578
//! sync-report bodies, `diff` classifies walk-vs-state deltas. 04-04 adds
//! `pub mod dav;` (HTTP client) and `poll_once` wiring here.

pub mod dav;
pub mod diff;
pub mod multistatus;

use std::sync::Arc;

use chrono::Utc;

use crate::config::ResolvedWatch;
use crate::error::Result;
use crate::pipeline;
use crate::store::Store;
use crate::vault::Vault;

use dav::DavClient;
use diff::{diff_walk, StateEntry, WalkEntry, WatchDelta};

/// One poll of one source: walk root → diff vs watch_state → download +
/// enqueue deltas. Returns the count of enqueued jobs. Errors are returned,
/// never panicked (the 04-06 interval loop owns WARN logging — D-11: no
/// secrets, no response bodies in logs).
///
/// v1 walking skeleton (04-04): single Depth:1 PROPFIND of the watch root;
/// full depth-capped BFS, removals and the sha256 no-op gate land in 04-05.
pub async fn poll_once(
    store: &Store,
    vault: &Arc<Vault>,
    src: &ResolvedWatch,
    max_fetch_bytes: u64,
) -> Result<usize> {
    let client = DavClient::new(src)?;
    let entries = client.propfind_children("").await?;
    // status-404 entries (sync removals) are ignored in v1 — sync lands 04-07.
    let walk: Vec<WalkEntry> = entries
        .iter()
        .filter(|e| e.status == 200)
        .map(WalkEntry::from)
        .collect();
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
    let now = Utc::now().to_rfc3339();

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
                &now,
            )
            .await?;
    }

    let mut enqueued = 0usize;
    for delta in deltas {
        // v1: Removed deltas are not acted on (removals land with the full
        // walk in 04-05); Skipped needs no action (skip-logging: 04-05).
        let entry = match delta {
            WatchDelta::Added(w) | WatchDelta::Changed(w) => w,
            _ => continue,
        };
        let bytes = client.get_bytes(&entry.path, max_fetch_bytes).await?;
        let sha = crate::pipeline::fetch::sha256_hex(&bytes);
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
        // else branch only fires for custom allowlist entries (no job in v1).
        if ext == "md" || ext == "txt" {
            let source_hint = format!("{}/{}", src.url.as_str().trim_end_matches('/'), entry.path);
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
        // Worst case after a crash between the two = one duplicate enqueue,
        // absorbed by the worker's sha256 no-op.
        store
            .upsert_watch_row(
                vault.id,
                &src.source_id,
                &entry.path,
                false,
                entry.etag.as_deref(),
                entry.size,
                Some(&sha),
                &now,
            )
            .await?;
    }

    store
        .record_sync_poll(vault.id, &src.source_id, Some(&now), None)
        .await?;
    Ok(enqueued)
}
