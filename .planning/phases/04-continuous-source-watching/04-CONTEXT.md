# Phase 4: Continuous Source Watching - Context

**Gathered:** 2026-10-31
**Status:** Ready for planning

> **Note:** The interactive gray-area selection timed out; the decisions below are
> Claude-recommended defaults (marked as such), not user-locked choices. The user
> may override any of them before/at plan review.

<domain>
## Phase Boundary

A configured WebDAV folder is polled continuously on an interval: new, changed, and
removed files are detected (etag/size, Depth:1 PROPFIND walk with a depth cap) and
ingestion jobs are enqueued automatically into the existing durable queue. Watcher
state (etags, sync/pageTokens, last-check) persists in SQLite so a restart resumes
watching with no full re-scan, and re-polling unchanged files enqueues nothing
(sha256-gated enqueue via the Phase 3 no-op).

No UI for source management (Phase 5), no lint/review queue (Phase 5), no Google
Drive or MCP source kinds (v2), no webhook/push-based watching.

Requirements: SRCC-01, SRCC-02.

</domain>

<decisions>
## Implementation Decisions

### Source configuration (recommended default — user may override)
- **D-01:** Watched sources are declared in `config.toml` as `[[watch]]` entries
  (url, credentials ref, poll interval, optional include globs). No watcher table
  in SQLite in v1 — Phase 5 UI edits the config surface later if needed.
- **D-02:** Credentials are basic-auth (the common WebDAV case) stored via the
  existing `SecretString` pattern, never logged (Phase 3 D-03 secrecy rules apply
  verbatim: no secret in errors, logs, or Debug).
- **D-03:** Operator-configured watch URLs are TRUSTED (self-hosted LAN servers
  are the target case): the Phase 3 SSRF public-IP guard does NOT apply to watch
  URLs — they may point at private addresses. HTTPS strongly encouraged, http://
  allowed for LAN with a config warning.

### Change detection (recommended default — user may override)
- **D-04:** Etag-first per-file detection: each poll does Depth:1 `PROPFIND` walks
  (recursive with a depth cap, default 4, never `Depth: infinity` per SRCC-01),
  compares per-file etag/size against persisted state; only unseen or
  changed files are downloaded and enqueued. Unchanged folders cost one cheap
  PROPFIND per level.
- **D-05:** RFC 6578 sync-token (`sync-collection` report) is used opportunistically
  when the server advertises it (DAV: sync capability); the sync token persists in
  the existing `sync_state.token` column and a 400/unsupported response falls back
  to plain PROPFIND forever after. This is what SRCC-02's "pageTokens" refers to.
- **D-06:** Per-file etag/size state persists in SQLite (planner decides: extend
  `sync_state` with a per-file row kind keyed by `(vault_id, source_id)` where
  source_id is the file URL, or a dedicated `watch_state` table in a new
  migration). Restart = zero re-scan of unchanged files (SRCC-02).
- **D-07:** Enqueue is content-gated end-to-end: etag/size gate avoids the
  download; the Phase 3 sha256 no-op (PIPE-02) is the final gate if a file is
  downloaded unchanged (servers with weak etags stay cheap).

### File handling
- **D-08:** Extension allowlist decides what is ingested: `.md`/`.txt` → new
  `ingest_raw` job kind (bytes → `raw/` directly, no fetch/SSRF step, reuses the
  `ingest_pdf` raw-copy pattern); `.pdf` → existing `ingest_pdf` path; anything
  else is skipped and noted once in `log.md`, not re-logged every poll.
- **D-09:** Removed files (present in watcher state, absent from walk): record a
  `log.md` removal entry + clean the watcher-state row. Existing vault pages are
  NEVER deleted or modified — the vault is append-biased; provenance to a vanished
  raw/ file is acceptable (recommended default — user may override toward
  "flag orphaned pages for Phase 5 review queue").

### Poller execution (recommended default — user may override)
- **D-10:** One interval task per source inside the daemon
  (`tokio::time::interval`, per-source interval, default 300 s, first poll on
  startup). The poller only walks + diffs + enqueues; all LLM work stays in the
  queue workers (backpressure comes free from the bounded Phase 2 pool).
- **D-11:** Poll failures are non-fatal and non-retried-instantly: record
  `last_error` in `sync_state`, log a WARN (no secrets, no response bodies),
  next attempt on the next tick. A crash mid-walk is safe: state is upserted
  per-source after each completed walk, so worst case one extra walk.
- **D-12:** Watcher starts/stops with the daemon lifecycle (same place the worker
  pool spawns); no separate process, no CLI subcommand beyond existing daemon.

### Claude's Discretion
- Exact PROPFIND XML parsing crate choice (`dav-server` client bits vs hand-rolled
  XML via `quick-xml`/`roxmltree`) — researcher should weigh maturity; pure-Rust
  preferred, no subprocess.
- Whether per-file state lives in `sync_state` vs a new `watch_state` table.
- Job payload schema for `ingest_raw` (mirror `ingest_pdf`'s shape).
- Depth-cap and interval config key names; jitter on first-poll to avoid thundering
  herd across multiple sources.
- Test doubles: a stub WebDAV server (raw TcpListener like `llm_provider_thinking`
  tests, or in-process axum PROPFIND responder in the TEST binary only — core
  stays axum-free per CI guard).

</decisions>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Project planning
- `.planning/ROADMAP.md` §Phase 4 — goal, success criteria (3), requirements SRCC-01/02
- `.planning/REQUIREMENTS.md` — SRCC-01, SRCC-02 wording (verbatim source of truth)
- `.planning/PROJECT.md` §Key Decisions — base-path decision, English artifacts, v1/v2 split (Drive/MCP → v2)

### Prior phase context (locked decisions that constrain Phase 4)
- `.planning/phases/03-extraction-pipeline/03-CONTEXT.md` — D-05/D-06/D-07/D-08 (raw/ conventions, drift, sync_state use), D-13 base-path
- `.planning/phases/02-llm-provider-durable-job-queue/02-CONTEXT.md` — queue/backoff/D-09/D-14 mock-only test rules
- `.planning/phases/03-extraction-pipeline/03-VALIDATION.md` + `03-SECURITY.md` — test/guard inventory to extend

### Existing code (integration points)
- `crates/saussunet-core/src/store/sync_state.rs` — watcher state schema + APIs ("behavior lands in Phase 4" is noted in the file header)
- `crates/saussunet-core/src/queue/mod.rs` — bounded worker pool, submit/claim APIs the poller enqueues through
- `crates/saussunet-core/src/pipeline/mod.rs` — `JOB_KIND_INGEST_URL`/`JOB_KIND_INGEST_PDF`, `submit_pdf` raw-copy pattern to mirror for `ingest_raw`, sha256 no-op gate
- `crates/saussunet-core/src/config.rs` — `FileConfig`/`RuntimeConfig` resolve pattern for new `[[watch]]` section
- `crates/saussunet/src/main.rs` — daemon bootstrap where watcher tasks spawn
- `scripts/check.sh` — CI guards the new code must not break (axum-absence in core, write-funnel, zero-real-LLM)

### External specs
- RFC 4918 (WebDAV) PROPFIND Depth:0/1 semantics — for the researcher
- RFC 6578 (WebDAV Distributed Locking/Collection Sync) `sync-collection` report — only if D-05 opportunistic sync-token is kept

</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- `store/sync_state.rs`: `upsert_sync_state`/`get_sync_state` with token/etag/last_poll_at/last_error columns — exactly the persistence SRCC-02 needs (may need per-file rows)
- `queue`: durable submit with atomic claim + backoff + dead-letter — poller enqueues through it, no new queue machinery
- `pipeline::submit_pdf` + `vault::raw::write_raw_binary`: raw-bytes ingestion pattern to mirror for `.md`/`.txt` files
- `vault::log::append_entry`: log.md entries for additions/removals (rotation handled)
- `config::SecretString` + FileConfig→RuntimeConfig resolve: `[[watch]]` section pattern
- `tests/llm_provider_thinking.rs` raw TcpListener stub-server pattern: template for a stub WebDAV server in tests

### Established Patterns
- Core crate is axum-free (CI guard) — PROPFIND client lives in core with plain reqwest; test stub servers use raw TcpListener or test-only deps
- TDD mandatory: RED first with MockProvider/stub servers; zero real LLM in CI (D-14 structural proof)
- `scripts/check.sh` is the gate; every plan's acceptance criteria must be runnable commands
- Secrets never in errors/logs/Debug (D-03 secrecy pattern) — applies to WebDAV basic-auth creds

### Integration Points
- Daemon bootstrap (`crates/saussunet/src/main.rs`): spawn one watcher task per configured `[[watch]]` source alongside worker pool
- `pipeline`: new `ingest_raw` job kind dispatched in the same kind-dispatch as `ingest_url`/`ingest_pdf`
- `sync_state` (or new `watch_state`): per-file etag/size persistence + per-source sync token

</code_context>

<specifics>
## Specific Ideas

- "The vault compounds on its own" (roadmap goal) — the poller must be boringly
  reliable: no manual re-trigger, no full re-scan after restart, quiet when
  nothing changed (zero log noise on no-op polls).
- LAN self-hosted WebDAV (e.g. a home server) is the primary deployment target —
  private-URL trust (D-03) matters more than internet-facing hardening.

</specifics>

<deferred>
## Deferred Ideas

- Google Drive source watcher — v2 (PROJECT.md)
- MCP stream / webhook-push watching — v2
- Watcher management + status in the Phase 5 UI (add/remove sources, last-poll
  dashboard, manual "poll now") — Phase 5
- Flagging vault pages orphaned by source deletion — possible Phase 5 review-queue
  feature if the user overrides D-09 later
- Fuzzy handling of case-insensitive/renamed files (rename = remove + add today)

</deferred>

---

*Phase: 04-Continuous Source Watching*
*Context gathered: 2026-10-31*
