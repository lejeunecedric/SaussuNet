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

/// One poll of one source: RFC 6578 sync-collection when the server
/// supports it (token round-trip, stale-token re-sync, latch on failure),
/// otherwise a depth-capped BFS walk → diff vs watch_state → download +
/// enqueue deltas → removals (complete walks only). Returns the
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
    // Shared per-poll context for delta application (sync + walk paths).
    let ctx = DeltaCtx {
        store,
        vault,
        src,
        client: &client,
        max_fetch_bytes,
        now,
    };

    // --- RFC 6578 sync-collection attempt (D-05, 04-07) -------------------
    // Discovery is try-and-fallback: the RFC adds no DAV: compliance token,
    // so we simply try. A latched `sync_unsupported` skips this forever.
    let sync_state = store.get_sync_state(vault.id, &src.source_id).await?;
    if !sync_state.as_ref().is_some_and(|s| s.sync_unsupported) {
        let stored = sync_state.and_then(|s| s.token).filter(|t| !t.is_empty());
        if let Some(n) = try_sync_poll(&ctx, stored.as_deref()).await? {
            return Ok(n);
        }
        // try_sync_poll latched sync_unsupported — fall through to the walk
        // (this poll AND every future one, D-05).
    }

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
    // H1 (04 review): a per-file failure is recorded and the loop CONTINUES —
    // one bad file must never wedge the source (files sorting after it would
    // never be ingested) nor drop the log lines of already-applied deltas.
    let mut file_err: Option<String> = None;
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
                let prior_sha = state
                    .iter()
                    .find(|s| s.path == entry.path)
                    .and_then(|s| s.sha256.as_deref());
                match apply_file_delta(&ctx, &entry, prior_sha, verb, &mut lines).await {
                    Ok(true) => enqueued += 1,
                    Ok(false) => {}
                    Err(e) => {
                        // D-11 hygiene: the error surface is pre-sanitized;
                        // strip newlines from the server-influenced path too.
                        let msg = sanitize_line(&e.to_string());
                        tracing::debug!(
                            "watch: {verb} {} failed: {msg}",
                            sanitize_line(&entry.path)
                        );
                        if file_err.is_none() {
                            file_err = Some(format!(
                                "{verb} {} failed: {msg}",
                                sanitize_line(&entry.path)
                            ));
                        }
                    }
                }
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
    // happened — a no-op poll is completely silent (T9). This runs even when a
    // per-file delta failed (H1): the applied deltas' lines are flushed
    // BEFORE the poll-level Err so they are never lost.
    if !lines.is_empty() {
        let header = format!("## [{}] update | Watch", Utc::now().format("%Y-%m-%d"));
        vault::log::append_entry(vault, &header, &lines).await?;
    }

    if let Some(msg) = walk_err.or(file_err) {
        return Err(CoreError::Dav(msg));
    }
    Ok(enqueued)
}

/// Shared per-poll context for delta application — one bundle for BOTH the
/// walk and the sync paths so the two call sites stay honest about the
/// crash-safe order (they share `apply_file_delta` verbatim).
#[derive(Clone, Copy)]
struct DeltaCtx<'a> {
    store: &'a Store,
    vault: &'a Arc<Vault>,
    src: &'a ResolvedWatch,
    client: &'a DavClient,
    max_fetch_bytes: u64,
    now: &'a str,
}

/// Attempt one RFC 6578 sync-collection poll (D-05 try-and-fallback).
/// `Ok(Some(n))` = sync worked, n jobs enqueued. `Ok(None)` = the server
/// does not support sync — `sync_unsupported` is now latched and the caller
/// must run the PROPFIND walk (this poll and every future one). `Err` =
/// sync IS supported but this poll failed; the token is NOT advanced, so
/// the next tick retries the same delta.
async fn try_sync_poll(ctx: &DeltaCtx<'_>, stored_token: Option<&str>) -> Result<Option<usize>> {
    let DeltaCtx {
        store,
        vault,
        src,
        client,
        ..
    } = *ctx;
    let report = match client.sync_collection("", stored_token).await {
        Ok(r) => r,
        Err(first) if stored_token.is_some() => {
            // Pitfall 5: stored tokens can expire (RFC 6578 §3.1). Retry
            // ONCE with an empty token (full re-sync — the sha256 gate makes
            // it cheap) before ANY latching.
            tracing::debug!(
                "watch sync: report with stored token failed ({first}); retrying with empty token"
            );
            match client.sync_collection("", None).await {
                Ok(r) => r,
                Err(second) if dav::is_sync_unsupported_signal(&second) => {
                    tracing::debug!(
                        "watch sync: empty-token resync also rejected ({second}); latching sync_unsupported"
                    );
                    store
                        .set_sync_unsupported(vault.id, &src.source_id, true)
                        .await?;
                    return Ok(None);
                }
                Err(second) => {
                    // M2: transport-level failure on the retry — do NOT
                    // latch; fail the poll so last_error records it and the
                    // next tick retries sync.
                    return Err(second);
                }
            }
        }
        Err(e) if dav::is_sync_unsupported_signal(&e) => {
            tracing::debug!(
                "watch sync: sync-collection unsupported ({e}); latching sync_unsupported"
            );
            store
                .set_sync_unsupported(vault.id, &src.source_id, true)
                .await?;
            return Ok(None);
        }
        Err(e) => {
            // M2: transport error on the first attempt (connection refused,
            // timeout, …) — the server may well support sync; record the
            // failure via last_error (caller) WITHOUT latching forever.
            tracing::debug!("watch sync: attempt failed on a transport error ({e}); not latching");
            return Err(e);
        }
    };
    let enqueued = apply_sync_report(ctx, &report).await?;
    // The token advances ONLY after the delta was fully applied: a crash
    // between apply and token replays the same delta next poll, absorbed by
    // the sha256 gate — the same at-least-once order as Pitfall 2.
    store
        .set_sync_token(vault.id, &src.source_id, report.token.as_deref())
        .await?;
    Ok(Some(enqueued))
}

/// Apply one sync-report delta. Shares the walk's gate/submit/upsert path
/// via `apply_file_delta`. CRITICAL (threat model): a sync delta is NOT a
/// complete universe — walk-style diff-based removals (Pitfall 3) must
/// NEVER run here; deletions come only from explicit 404-status items.
async fn apply_sync_report(ctx: &DeltaCtx<'_>, report: &dav::SyncReport) -> Result<usize> {
    let DeltaCtx {
        store,
        vault,
        src,
        client: _,
        max_fetch_bytes: _,
        now,
    } = *ctx;
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

    let mut lines: Vec<String> = Vec::new();
    let mut enqueued = 0usize;

    // 200-status items = added/changed candidates (sync-level infinity
    // reports the whole subtree, so paths may be nested).
    let items: Vec<WalkEntry> = report
        .entries
        .iter()
        .filter(|e| e.status == 200)
        .map(WalkEntry::from)
        .collect();
    // Collections among them: state bookkeeping rows only, never enqueued.
    for w in items.iter().filter(|w| w.is_dir) {
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
    let files: Vec<WalkEntry> = items.into_iter().filter(|w| !w.is_dir).collect();
    // H1 (04 review): same per-file isolation as the walk loop — a failing
    // file is recorded, the remaining deltas still apply, and `lines` is
    // flushed before the Err (the token does NOT advance, so the failed file
    // replays next poll; the sha256 gate absorbs the applied ones).
    let mut file_err: Option<String> = None;
    for delta in diff_walk(&files, &state, &src.extensions) {
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
                let prior_sha = state
                    .iter()
                    .find(|s| s.path == entry.path)
                    .and_then(|s| s.sha256.as_deref());
                match apply_file_delta(ctx, &entry, prior_sha, verb, &mut lines).await {
                    Ok(true) => enqueued += 1,
                    Ok(false) => {}
                    Err(e) => {
                        let msg = sanitize_line(&e.to_string());
                        tracing::debug!(
                            "watch sync: {verb} {} failed: {msg}",
                            sanitize_line(&entry.path)
                        );
                        if file_err.is_none() {
                            file_err = Some(format!(
                                "sync {verb} {} failed: {msg}",
                                sanitize_line(&entry.path)
                            ));
                        }
                    }
                }
            }
            WatchDelta::Skipped(w) => {
                // Same once-only marker-row discipline as the walk (D-08).
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
            WatchDelta::Removed(_) => {
                // IGNORED by design: diff_walk's Removed means "in state,
                // absent from THIS delta" — meaningless for a partial sync
                // universe. Only explicit 404 items below remove state.
            }
        }
    }

    // 404-status items = server-reported deletions (free with sync, D-05).
    // Same handling as walk removals: log line + state prune; vault pages
    // are never touched (D-09).
    for e in report.entries.iter().filter(|e| e.status == 404) {
        store
            .delete_watch_row(vault.id, &src.source_id, &e.path)
            .await?;
        lines.push(format!("- removed: {}", sanitize_line(&e.path)));
    }

    if !lines.is_empty() {
        let header = format!("## [{}] update | Watch", Utc::now().format("%Y-%m-%d"));
        vault::log::append_entry(vault, &header, &lines).await?;
    }
    // H1: flush the applied deltas' lines first, THEN surface the failure —
    // try_sync_poll's `?` keeps the token frozen so the failed file replays.
    if let Some(msg) = file_err {
        return Err(CoreError::Dav(msg));
    }
    Ok(enqueued)
}

/// Shared Added/Changed application for BOTH the walk and the sync path:
/// GET → sha256 gate (D-07) → route + submit → state row upsert AFTER the
/// durable job returned (Pitfall 2 crash-safe order). Returns `true` iff a
/// job was enqueued — the sha256 gate and non-ingestible custom allowlist
/// entries enqueue nothing.
async fn apply_file_delta(
    ctx: &DeltaCtx<'_>,
    entry: &WalkEntry,
    prior_sha: Option<&str>,
    verb: &str,
    lines: &mut Vec<String>,
) -> Result<bool> {
    let DeltaCtx {
        store,
        vault,
        src,
        client,
        max_fetch_bytes,
        now,
    } = *ctx;
    let filename = entry
        .path
        .rsplit('/')
        .next()
        .unwrap_or(&entry.path)
        .to_string();
    // H1 dotfile edge: a name like ".md" has an EMPTY stem — the pipeline
    // sanitizer refuses it, which would fail this file forever and (before
    // the H1 catch) wedge every later file. Note it once as a skip instead:
    // the marker row (sha256 NULL) suppresses repeat notes like D-08 does.
    if filename
        .rsplit_once('.')
        .is_some_and(|(stem, _)| stem.is_empty())
    {
        lines.push(format!(
            "- skipped (empty file stem): {}",
            sanitize_line(&entry.path)
        ));
        store
            .upsert_watch_row(
                vault.id,
                &src.source_id,
                &entry.path,
                false,
                entry.etag.as_deref(),
                entry.size,
                None,
                now,
            )
            .await?;
        return Ok(false);
    }
    let bytes = client.get_bytes(&entry.path, max_fetch_bytes).await?;
    let sha = crate::pipeline::fetch::sha256_hex(&bytes);
    // D-07 sha256 backstop: identical bytes under a rotated (weak) etag
    // update the row only — no job, no log line.
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
        return Ok(false);
    }
    let ext = filename
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    // D-08 routing: .md/.txt → ingest_raw, .pdf → ingest_pdf. diff_walk
    // already dropped anything outside the allowlist (Skipped), so the
    // else branch only fires for custom allowlist entries (no job).
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
        return Ok(false);
    }
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
    Ok(true)
}

/// Log/state-line injection guard: untrusted server paths (and error text
/// built from them) must never carry a newline into log.md or last_error.
fn sanitize_line(s: &str) -> String {
    s.replace(['\r', '\n'], " ")
}

/// Spawn the interval poll loop for one configured source (D-10): polls
/// immediately on startup (`interval`'s first tick fires instantly) and
/// forever after; a failing poll is WARN-only (D-11 — `poll_once` errors are
/// pre-sanitized: no creds, no bodies) and the next attempt simply rides the
/// next tick (the interval IS the retry loop; no in-poller backoff).
///
/// First-poll jitter (RESEARCH open Q5): deterministic stagger —
/// `hash(source_id) % 5000 ms`, capped at `interval / 10` so short-interval
/// tests stay fast. No rand dep, no thundering-herd machinery — personal-LAN
/// scale.
///
/// Silent-watcher posture (threat model): a panic inside this task would stop
/// one source invisibly; the caller keeps the `JoinHandle` and aborts it on
/// shutdown — same posture as the Phase 2 worker pool. Phase 5 UI surfaces
/// last-poll status from `sync_state`.
pub fn spawn_watcher(
    store: Arc<Store>,
    vault: Arc<Vault>,
    src: ResolvedWatch,
    max_fetch_bytes: u64,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let jitter = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            src.source_id.hash(&mut h);
            let ms = h.finish() % 5000;
            std::time::Duration::from_millis(ms.min((src.interval / 10).as_millis() as u64))
        };
        tokio::time::sleep(jitter).await;
        tracing::debug!(source = %src.source_id, interval = ?src.interval, "watch poller started");
        let mut tick = tokio::time::interval(src.interval); // first tick fires immediately (D-10)
        loop {
            tick.tick().await;
            match poll_once(&store, &vault, &src, max_fetch_bytes).await {
                // DEBUG only on success — no per-file logging from the loop.
                Ok(n) => tracing::debug!(source = %src.source_id, enqueued = n, "watch poll ok"),
                // D-11: WARN only — poll_once errors are pre-sanitized
                // (no creds, no response bodies); next attempt rides the
                // next tick.
                Err(e) => {
                    tracing::warn!(source = %src.source_id, "watch poll failed: {e}");
                }
            }
        }
    })
}
