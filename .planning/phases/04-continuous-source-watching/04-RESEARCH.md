# Phase 4: Continuous Source Watching — Research

**Researched:** 2026-11-01
**Domain:** WebDAV client (RFC 4918 PROPFIND / RFC 6578 sync-collection) + durable polling/watcher state on the existing queue + SQLite stack
**Confidence:** HIGH (stack + integration points verified against crates.io/docs.rs/live code; RFCs fetched directly)

<user_constraints>
## User Constraints (from CONTEXT.md)

**Note from CONTEXT.md:** the decisions below are Claude-recommended defaults (interactive selection timed out); the user may override at plan review. Treat as locked for planning unless overridden.

### Locked Decisions
- **D-01:** Watched sources declared in `config.toml` as `[[watch]]` entries (url, credentials ref, poll interval, optional include globs). No watcher table in SQLite in v1.
- **D-02:** Basic-auth credentials via the existing `SecretString` pattern; never in errors, logs, or Debug (Phase 3 D-03 secrecy rules verbatim).
- **D-03:** Watch URLs are operator-trusted (LAN private IPs allowed); the Phase 3 SSRF public-IP guard does NOT apply. HTTPS encouraged; `http://` allowed with a config warning.
- **D-04:** Etag-first per-file detection: Depth:1 `PROPFIND` walks (recursive with depth cap, default 4, never `Depth: infinity`), compare per-file etag/size vs persisted state; only unseen/changed files are downloaded + enqueued.
- **D-05:** RFC 6578 `sync-collection` used opportunistically when supported; token persists in existing `sync_state.token`; 400/unsupported → plain PROPFIND forever after. This is SRCC-02's "pageTokens".
- **D-06:** Per-file etag/size state persists in SQLite (planner decides: extend `sync_state` vs new `watch_state` table). Restart = zero re-scan (SRCC-02).
- **D-07:** Enqueue content-gated end-to-end: etag/size gate avoids the download; Phase 3 sha256 no-op (PIPE-02) is the final gate if downloaded unchanged.
- **D-08:** Extension allowlist: `.md`/`.txt` → new `ingest_raw` job kind (bytes → `raw/` directly, mirrors `ingest_pdf` raw-copy pattern); `.pdf` → existing `ingest_pdf`; anything else skipped, noted once in `log.md`, not re-logged every poll.
- **D-09:** Removed files → `log.md` removal entry + delete watcher-state row. Vault pages are NEVER deleted/modified.
- **D-10:** One interval task per source in the daemon (`tokio::time::interval`, default 300 s, first poll on startup). Poller only walks + diffs + enqueues; LLM work stays in queue workers.
- **D-11:** Poll failures non-fatal, no instant retry: record `last_error` in `sync_state`, WARN (no secrets, no response bodies), next attempt next tick. State upserted per-source after each completed walk.
- **D-12:** Watcher starts/stops with daemon lifecycle (same place worker pool spawns); no new process, no new CLI subcommand.

### Claude's Discretion
- PROPFIND XML parsing crate choice (client crate vs hand-rolled `quick-xml`/`roxmltree`) — pure-Rust preferred, no subprocess. → **Answered in Standard Stack below.**
- Per-file state in `sync_state` vs new `watch_state` table. → **Answered in Migration section.**
- `ingest_raw` job payload schema (mirror `ingest_pdf`). → **Answered in Integration Points.**
- Depth-cap/interval config key names; first-poll jitter. → **Suggested in Config section.**
- Test doubles: stub WebDAV server (raw TcpListener vs test-only axum). → **Answered in Validation Architecture.**

### Deferred Ideas (OUT OF SCOPE)
- Google Drive source watcher (v2), MCP stream / webhook push (v2), watcher management UI (Phase 5), orphaned-page flagging (possible Phase 5), fuzzy rename handling (rename = remove + add today).
</user_constraints>

<architectural_responsibility_map>
## Architectural Responsibility Map

Single-binary self-hosted server; the phase is a background-loop feature, not a UI/API feature.

| Capability | Primary Tier | Secondary Tier | Rationale |
|------------|-------------|----------------|-----------|
| WebDAV polling (PROPFIND walk, sync report, GET download) | Backend (tokio task in daemon, `saussunet-core::watch` module) | — | Outbound HTTP client work; core stays axum-free (client ≠ server dep, same precedent as `pipeline::fetch`) |
| Watcher state persistence (etags/sizes/token/last-check) | Database/Storage (SQLite via `Store`) | — | SRCC-02 explicitly requires SQLite persistence; DB is the rebuildable projection (Phase 1 D-01) |
| Change diffing + gated enqueue | Backend (poller task) | Database (queue `jobs` table) | Poller diffs walk vs state, then `insert_job` — same submit path as `submit_pdf` |
| `ingest_raw` job handling | Backend (queue worker via `pipeline::handle_job` kind dispatch) | — | All LLM work stays in the bounded worker pool (D-10, backpressure free) |
| `log.md` additions/removals/skip notes | Storage (vault FS through the `vault::log` funnel) | — | PIPE-06 write funnel; never raw fs writes |
| Source configuration | Config (`config.toml` `[[watch]]` → `RuntimeConfig`) | — | D-01; Phase 5 UI will later edit this surface |
</architectural_responsibility_map>

<research_summary>
## Summary

Researched the Rust WebDAV-client ecosystem, RFC 6578 sync-collection feasibility, the exact integration points in the current codebase, and a CI-compatible test strategy for a stub WebDAV server.

**Headline finding on the stack:** the previously-suggested `reqwest_dav` is a trap for this workspace. Its current release (0.3.3, 2026-03) depends on `reqwest ^0.13`, while the workspace deliberately pins `reqwest 0.12` (MoriNet QUAL-05 alignment — two reqwest majors in one tree is exactly what the workspace Cargo.toml comment and cargo-deny posture forbid). Its reqwest-0.12-compatible line (0.2.2) has **no sync-collection support** (D-05 would need hand-rolled POST anyway), wraps its own reqwest client (bypassing the established `fetch.rs` size-cap/redirect discipline), and drags `serde-xml-rs`, `digest_auth`, `http`, `httpdate` for a protocol surface that is literally three requests: PROPFIND, GET, and an optional sync REPORT-as-POST. **Recommendation: hand-roll a small `watch/dav.rs` client on the existing reqwest 0.12 + one tiny XML parser (`roxmltree`).** HERMES.md's own stack row rated reqwest_dav Medium confidence with "manual PROPFIND with reqwest + quick-xml" as the named alternative.

**Sync-token verdict:** keep D-05's opportunistic support, but understand discovery correctly: RFC 6578 adds **no compliance-class token to the `DAV:` header** — support is advertised only via the `DAV:supported-report-set` property, which itself requires a REPORT to read (chicken-and-egg). The pragmatic pattern every real client uses is **try-and-fallback**: first poll sends `sync-collection` with an empty token; any non-207 (400/403/405/501) marks the source sync-unsupported in state and it uses the PROPFIND walk forever after. The value is real on big trees (deltas + server-reported deletions), and the fallback path is the plain walk you must build anyway — so the marginal cost is one request shape + one response branch.

**Primary recommendation:** Hand-rolled `watch` module in core (reqwest 0.12 + roxmltree 0.21), new `watch_state` table in migration 0009 for per-file etag/size rows (sync_state keeps per-source token/last_poll_at/last_error + a `sync_unsupported` flag column), raw-TcpListener stub server in `crates/saussunet-core/tests/` following the `llm_provider_thinking.rs` pattern (axum is impossible even as a dev-dep — the CI guard greps the whole Cargo.toml).
</research_summary>

<standard_stack>
## Standard Stack

### Core (all already in the workspace except roxmltree)
| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| reqwest | 0.12 (workspace-pinned) | PROPFIND/GET/sync-collection HTTP, basic auth, redirects | Already a core dep (`pipeline::fetch` precedent); `client.request(Method::from_bytes(b"PROPFIND"), url)` supports arbitrary methods natively; `.basic_auth(user, Some(pass))` built in; `StatusCode::MULTI_STATUS` (207) is a constant |
| roxmltree | 0.21.2 (NEW, ~4k LOC, MIT OR Apache-2.0, updated 2026-10-24) | Parse 207 multistatus + sync-report XML bodies | Read-only DOM in one call — multistatus bodies are small, so the pull-parser streaming advantage is irrelevant; namespace-aware (`namespace_uri() == "DAV:"`) which multistatus parsing *requires*; far less code than a quick-xml event loop for "find all d:response nodes" |
| tokio | 1.54 (workspace) | `tokio::time::interval` poll loop, task spawn/abort with daemon lifecycle | D-10/D-12; established worker-loop pattern in `queue/mod.rs` |
| sqlx | 0.8 (workspace) | `watch_state` table + store APIs | Every store module pattern |
| sha2, chrono, url, serde_json, tracing, thiserror | workspace pins | sha256 gate, timestamps, href joining, payloads, logging, errors | Existing patterns |

### Supporting
| Library | Version | Purpose | When to Use |
|---------|---------|---------|-------------|
| tempfile | 3 (already dev-dep) | Temp vault + temp SQLite for integration tests | Every test |
| glob (0.3, optional NEW) | 0.3.x | D-01 "optional include globs" matching on relative paths | Only if globs ship in v1; extension allowlist (D-08) alone may suffice — planner may defer globs |

### Alternatives Considered
| Instead of | Could Use | Tradeoff |
|------------|-----------|----------|
| Hand-rolled reqwest+roxmltree | `reqwest_dav` 0.3.3 | **Blocked:** pins `reqwest ^0.13` → duplicate reqwest majors vs the workspace 0.12 pin (violates the QUAL-05 single-version contract the workspace exists to enforce). Also no sync-collection, own client, +4 transitive deps |
| Hand-rolled | `reqwest_dav` 0.2.2 | reqwest ^0.12 compatible and maintained (2025-08), but still: no sync-collection (D-05 needs hand-rolled POST regardless), no REPORT, own client config (redirect/timeout discipline forks from `fetch.rs`), serde-xml-rs deserialization of multistatus is opaque for edge cases (per-item `<d:status>`/`<d:error>`). Only worth it if the team refuses any hand-rolled protocol code |
| Hand-rolled | `webdav-request` 0.4.0 | 6.4k total downloads, self-described "under development and not stable", no sync support. Reject |
| Hand-rolled | `dav-client` (max246) | Does not exist on crates.io (git-only, sync client) — already rejected in HERMES.md |
| roxmltree 0.21 | quick-xml 0.42 | quick-xml is faster/streaming and more popular, but it's a pull parser: multistatus parsing becomes a manual event-state-machine (~3× the code for this use). 0.42 is edition-2024/rust-version 1.86 — fine, but no benefit here. Either passes; roxmltree recommended for minimal surface. If the planner prefers one-parser-crate-fits-all, quick-xml 0.41 (edition 2021) is the fallback |

**Installation:**
```toml
# Cargo.toml [workspace.dependencies]
roxmltree = "0.21"
# crates/saussunet-core/Cargo.toml [dependencies]
roxmltree = { workspace = true }
```
No new dev-dependencies needed (stub server = std + tokio, both already present).
</standard_stack>

<protocol_notes>
## WebDAV Protocol Facts the Planner Needs (Q3)

### PROPFIND request (per collection, Depth: 1)
```
PROPFIND /remote.php/dav/files/user/vault-inbox/ HTTP/1.1
Depth: 1
Authorization: Basic …
Content-Type: application/xml
<?xml version="1.0"?>
<d:propfind xmlns:d="DAV:">
  <d:prop>
    <d:getetag/><d:getcontentlength/><d:resourcetype/>
  </d:prop>
</d:propfind>
```
- Response: **207 Multi-Status**, one `<d:response>` per resource **including the requested collection itself first** (Depth:1 includes self — skip it when diffing).
- reqwest: `client.request(reqwest::Method::from_bytes(b"PROPFIND")?, url).header("Depth", "1").body(xml)`.
- **Fields needed per response item:**
  - `<d:href>` — a URI path, **percent-encoded** (spaces → `%20`, non-ASCII UTF-8-encoded). Relativize: percent-decode, then strip the walked collection's path prefix (compare on decoded paths; rebuild child URL by joining decoded segments onto the base `Url` with `url::Url::join` per segment, re-encoding naturally). Trailing `/` on href ⇒ collection (cheap heuristic; combine with resourcetype).
  - `<propstat>` with `<getetag>` (quoted or bare; some servers weak-etag it `W/"…"`) and `<getcontentlength>` (absent on collections).
  - `<resourcetype><d:collection/></resourcetype>` — authoritative collection detection.
  - A property may come back in a `<propstat>` with `<status>HTTP/1.1 404 Not Found>` (unsupported prop) — only trust props under the 200-propstat.
- Depth cap walk: BFS from root, one Depth:1 PROPFIND per collection, stop descending at `depth_cap` (default 4). Never send `Depth: infinity` (SRCC-01).
- Auth: basic auth preemptively on every request (`.basic_auth()` does this) — don't rely on 401-challenge replay; reqwest handles it either way.

### sync-collection report (RFC 6578, D-05 opportunistic)
```
POST <collection-url>            Depth: 0 (report only defined at Depth 0)
Content-Type: application/xml
<?xml version="1.0" encoding="utf-8"?>
<d:sync-collection xmlns:d="DAV:">
  <d:sync-token></d:sync-token>          <!-- empty = initial full sync -->
  <d:sync-level>infinity</d:sync-level>  <!-- see note -->
  <d:prop><d:getetag/><d:getcontentlength/></d:prop>
</d:sync-collection>
```
- 207 response: `<d:response>` per changed member (200-status = added/changed with fresh etag; **404-status = removed** — this gives deletions for free) plus a trailing `<d:sync-token>` (opaque URI string) to persist in `sync_state.token`.
- **sync-level note:** `infinity` at the watch root returns the whole subtree in one report — this is *not* PROPFIND `Depth: infinity` (SRCC-01 constrains PROPFIND Depth), and it keeps the design to ONE token per source (matches D-05's single `sync_state.token`). If the planner wants strict symmetry with the walk, `sync-level: 1` per collection works but needs per-collection tokens (schema change) — not recommended for v1.
- **Discovery:** no `DAV:` header compliance class exists for sync; `supported-report-set` requires a REPORT to read. So: try once (initial sync, empty token); any non-207 → set `sync_unsupported` flag, fall back to PROPFIND walk forever (D-05). Also handle *stale token*: server returns 400/`<d:valid-interface/>`-style error for expired tokens → re-run with empty token (full re-sync) once before flagging unsupported.
- Truncation (`<d:truncated-requests>` / intermediate token, RFC 6578 §3.6): v1 may treat "intermediate sync-token" as "loop the report with the new token until no intermediate token" — or simply persist the intermediate token and continue next poll (simplest correct-enough behavior).

### Downloads
Plain `GET` with basic auth; size-cap the body like `fetch.rs` does (reuse `DEFAULT_MAX_FETCH_BYTES` pattern or a watch-specific cap); `ETag` response header equals the walked etag (compare and prefer the GET's actual sha256 for the gate — D-07).
</protocol_notes>

<codebase_integration>
## Integration Points — Exact Signatures (Q4)

All verified live in the repo (2026-11-01). Planner: these are the seams Phase 4 plugs into.

### Queue submit (poller → queue)
```rust
// crates/saussunet-core/src/store/jobs.rs:58
pub async fn insert_job(&self, vault_id: i64, kind: &str, payload_json: &str) -> Result<i64>
```

### Pipeline kind dispatch (add `ingest_raw` arm here)
```rust
// crates/saussunet-core/src/pipeline/mod.rs
pub const JOB_KIND_INGEST_URL: &str = "ingest_url";
pub const JOB_KIND_INGEST_PDF: &str = "ingest_pdf";           // + new: JOB_KIND_INGEST_RAW = "ingest_raw"
pub async fn handle_job(store: &Store, vault: &Arc<Vault>, provider: &Arc<dyn LlmProvider>,
                        job: &Job, _claim_token: &str) -> Result<()>   // match on job.kind; unknown kind dead-letters after ladder
```
`ingest_pdf` payload pattern to mirror for `ingest_raw`:
```rust
pub struct IngestPdfPayload { pub raw_path: String, pub source_id: String, pub filename: String }
// submit_pdf(store: &Store, vault: &Vault, bytes: &[u8], filename: &str,
//            max_upload_bytes: u64) -> Result<(i64, String)>
// — validates magic/extension, sanitize_component(slug), write_raw_binary → raw/papers/…,
//   payload {raw_path, source_id, filename}, insert_job. ingest_raw mirrors this WITHOUT
//   the %PDF magic check (text bytes), subdir e.g. "watch" or None.
```
The `ingest_raw` worker half reads the raw copy (path-guarded `rel.starts_with("raw")` like `ingest_pdf`), computes sha256, `set_source_sha256`, then `run_extraction_passes(...)`. Note: the sha256 no-op gate currently lives in `ingest_url` only; for watch-driven files the **poller** is the sha256 gate (it compares downloaded-bytes sha256 against `watch_state.sha256` before enqueueing), so `ingest_raw` itself can be a straight re-ingest — but keeping a cheap `get_source_sha256` no-op inside `ingest_raw` too is free defense for re-queued jobs.

### sync_state store (per-source watcher state)
```rust
// crates/saussunet-core/src/store/sync_state.rs
pub struct SyncState { pub vault_id: i64, pub source_id: String, pub token: Option<String>,
                       pub etag: Option<String>, pub last_poll_at: Option<String>,
                       pub last_error: Option<String> }
pub async fn upsert_sync_state(&self, vault_id: i64, source_id: &str, token: Option<&str>,
    etag: Option<&str>, last_poll_at: Option<&str>, last_error: Option<&str>) -> Result<()>
pub async fn get_sync_state(&self, vault_id: i64, source_id: &str) -> Result<Option<SyncState>>
pub async fn set_source_sha256(&self, vault_id: i64, source_id: &str, sha256: &str) -> Result<()>
pub async fn get_source_sha256(&self, vault_id: i64, source_id: &str) -> Result<Option<String>>
```
⚠️ `upsert_sync_state` overwrites ALL columns — a poller that writes `last_poll_at` must re-pass the existing token/etag (read-modify-write) or the planner should add a narrower upsert variant in migration-0009's module.

### Vault write funnel (poller writes only these)
```rust
// crates/saussunet-core/src/vault/raw.rs
pub async fn write_raw(vault: &Vault, subdir: Option<&str>, slug: &str, ext: &str,
                       frontmatter: &RawFrontmatter, body: &[u8]) -> Result<PathBuf>
pub async fn write_raw_binary(vault: &Vault, subdir: Option<&str>, slug: &str, ext: &str,
                              body: &[u8]) -> Result<PathBuf>
pub struct RawFrontmatter { pub source_url: String, pub ingested: String, pub sha256: String }
// crates/saussunet-core/src/vault/log.rs
pub async fn append_entry(vault: &Vault, header: &str, lines: &[String]) -> Result<()>
```
Watch poller writes: raw copies via `write_raw*` (allowed by the write-funnel guard — `vault/raw.rs` is whitelisted), log entries via `append_entry`. **No other fs writes anywhere in core src** (check.sh greps `fs::write|File::create`).

### Config resolve pattern (add `[[watch]]`)
```rust
// crates/saussunet-core/src/config.rs
pub struct FileConfig { /* server, storage, vault, llm, pipeline */ }   // + #[serde(default)] pub watch: Vec<WatchSection>
pub struct SecretString(String);  // Debug renders "***"; .reveal() is the only read
impl FileConfig { pub fn resolve(&self) -> Result<RuntimeConfig> }      // validate + env-override here
```
Suggested shape (names are discretion — keep defaults per D-10/D-04):
```toml
[[watch]]
url = "https://nas.lan/dav/vault-inbox/"
username = "saussu"
password = "…"            # SecretString; consider SAUSSUNET_WATCH__<N>__PASSWORD env override parity
interval_secs = 300        # D-10 default
depth_cap = 4              # D-04 default
extensions = ["md", "txt", "pdf"]   # D-08 default allowlist
```
**Hard coupling:** `default_config()` in `lib.rs` is byte-identical-tested against `config.example.toml` (`committed_example_config_matches_default_config`). Any template change (e.g. a commented `[[watch]]` example) MUST update both files in the same commit. Also mirror the WR-02 validation discipline: `interval_secs = 0` / `depth_cap = 0` should be rejected at `resolve()`.

### Daemon bootstrap (spawn watcher tasks here)
```rust
// crates/saussunet/src/main.rs — boot(): config → boot_open → provider → spawn_workers → axum serve → ctrl_c → abort handles
// Phase 4 inserts between worker spawn and the axum serve:
//   for each resolved watch source: tokio::spawn(watch::spawn_watcher(store.clone(), vault.clone(), provider?, source_cfg, watch_cfg))
//   collect JoinHandles; abort alongside `handles` on ctrl_c (D-12)
```
Note the WR-03 precedent: with the default `mock` provider the queue is deliberately not drained. The **poller should still run** (walking + enqueueing is provider-independent; jobs just stay pending) — but consider gating `ingest_raw`/`ingest_pdf` enqueue on the same `daemon_drains_queue(&rt)` check to avoid unbounded pending growth under mock, or just let it enqueue (planner's call; jobs are durable and cheap).

### Migration numbering
Latest existing: `migrations/0008_proposal_reason_validation.sql`. **Next free number: 0009.** Migrations live at workspace root, auto-applied by `sqlx::migrate!`.
</codebase_integration>

<database_design>
## Migration 0009 — Table Decision (Q6)

**Recommendation: dedicated `watch_state` table; do NOT overload `sync_state`.**

Why not `sync_state` per-file rows: its PK is `(vault_id, source_id)` and Phase 3 already owns `source_id = submitted URL` semantics there (sha256 drift column, `set_source_sha256`). Mixing per-file watch rows in would (a) make `get_sync_state` results ambiguous for the pipeline, (b) force the file URL into the drift-tracking namespace and cause accidental sha256-gate interactions, (c) `upsert_sync_state`'s all-column overwrite is hostile to partial per-file updates. Per-source rows already exist in sync_state — keep them for exactly that: per-source.

```sql
-- 0009_watch_state.sql — per-file watcher state for WebDAV sources (SRCC-02).
CREATE TABLE IF NOT EXISTS watch_state (
    vault_id   INTEGER NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
    source_id  TEXT NOT NULL,           -- watch source id (config url, verbatim)
    path       TEXT NOT NULL,           -- decoded path relative to watched root, '/' separators, no leading '/'
    is_dir     INTEGER NOT NULL DEFAULT 0,
    etag       TEXT,
    size       INTEGER,
    sha256     TEXT,                    -- digest of last ingested bytes (D-07 gate)
    last_seen_at TEXT NOT NULL,        -- last poll that observed it (removal detection)
    PRIMARY KEY (vault_id, source_id, path)
);
-- Per-source sync_state gains the sync-unsupported flag (D-05 fallback latch):
ALTER TABLE sync_state ADD COLUMN sync_unsupported INTEGER NOT NULL DEFAULT 0;
```
Store module `store/watch_state.rs` (pattern: `store/sync_state.rs`): `upsert_watch_row`, `list_watch_rows(vault_id, source_id)`, `delete_watch_row`, and a `replace_watch_rows` transaction helper (diff-and-swap after a completed walk keeps D-11's "state upserted after each completed walk" simple and atomic). Removal detection = rows present in state but absent from the completed walk set (only after a walk that reached full depth without error — a partial walk must not mass-delete state).
</database_design>

<architecture_patterns>
## Architecture Patterns

### System Architecture Diagram

```
config.toml [[watch]] ──resolve()──► RuntimeConfig.watch[]
      │
      ▼ (daemon boot, one task per source)
tokio::time::interval(300s) ──tick──► POLLER TASK (core::watch)
      │
      ├─ has sync_state.token && !sync_unsupported?
      │      yes ─► POST sync-collection(token) ──207──► delta set {path, etag, size, 200|404}
      │      no  ─► BFS Depth:1 PROPFIND walk (depth cap 4) ──► full set {path, etag, size}
      │                    └─ first attempt w/ empty token fails non-207 ─► latch sync_unsupported, use walk
      ▼
DIFF vs watch_state rows
      ├─ unchanged (etag+size match) ─────────────► nothing (zero log noise)
      ├─ new/changed file, ext ∈ allowlist ─► GET bytes ─► sha256 ─► == watch_state.sha256? ─yes─► update row only
      │                                                        └no─► write_raw(_binary) ─► insert_job(ingest_raw|ingest_pdf)
      │                                                              ─► upsert watch_state row ─► log.md add entry
      ├─ ext ∉ allowlist ─► log.md note ONCE (persist "skipped" marker in row) — not re-logged
      ├─ in state, absent from walk ─► log.md removal entry ─► delete row (vault pages untouched, D-09)
      ▼
after completed walk: upsert sync_state(token, last_poll_at, last_error=None)   [failure: last_error + WARN, next tick]
      ▼
durable jobs queue ─► existing bounded worker pool ─► pipeline::handle_job("ingest_raw"/"ingest_pdf") ─► vault writer funnel
```

### Recommended module layout
```
crates/saussunet-core/src/watch/
├── mod.rs        # WatchSource config struct, spawn_watcher() interval loop, poll_once() orchestration
├── dav.rs        # hand-rolled client: propfind(), sync_collection(), get_bytes(); DavEntry {href, etag, size, is_collection}
├── multistatus.rs# roxmltree parsing → Vec<DavEntry> (+ sync-report variant: per-item status 200 vs 404)
└── diff.rs       # walk-set vs watch_state diff → Vec<WatchDelta::{Added,Changed,Removed,Skipped}> (pure fn — unit-testable without HTTP)
crates/saussunet-core/src/store/watch_state.rs   # SQL CRUD (pattern: store/sync_state.rs)
crates/saussunet-core/tests/watch_webdav.rs      # integration tests vs stub server
```

### Pattern 1: interval loop per source (mirrors worker_loop shape)
```rust
// Source: established pattern in crates/saussunet-core/src/queue/mod.rs worker_loop
pub fn spawn_watcher(store: Arc<Store>, vault: Arc<Vault>, src: ResolvedWatch) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(src.interval); // first tick fires immediately = D-10 "first poll on startup"
        loop {
            tick.tick().await;
            if let Err(e) = poll_once(&store, &vault, &src).await {
                // D-11: WARN only — no secrets, no response bodies; last_error already recorded inside poll_once
                tracing::warn!(source = %src.id, "watch poll failed: {e}");
            }
        }
    })
}
```
### Pattern 2: custom-method PROPFIND with reqwest
```rust
// Source: reqwest docs (RequestBuilder::method / Method::from_bytes); RFC 4918 §9.1
let body = r#"<?xml version="1.0"?><d:propfind xmlns:d="DAV:"><d:prop>\
              <d:getetag/><d:getcontentlength/><d:resourcetype/></d:prop></d:propfind>"#;
let resp = client.request(reqwest::Method::from_bytes(b"PROPFIND")?, url.clone())
    .header("Depth", "1")
    .basic_auth(user, Some(pass))
    .header(reqwest::header::CONTENT_TYPE, "application/xml")
    .body(body.to_string())
    .send().await?;
if resp.status() != reqwest::StatusCode::MULTI_STATUS { /* treat as poll failure */ }
let xml = resp.text().await?; // size-cap before parse on big trees: read body with cap like fetch.rs
```
### Pattern 3: multistatus parse with roxmltree
```rust
// Source: roxmltree docs (docs.rs/roxmltree) — namespace-aware read-only tree
let doc = roxmltree::Document::parse(xml)?;
let dav = "DAV:";
for resp in doc.descendants().filter(|n| n.has_tag_name("response") && n.namespace_uri() == Some(dav)) {
    let href = resp.children().find(|n| n.has_tag_name("href") && n.namespace_uri() == Some(dav))
        .and_then(|n| n.text()).unwrap_or_default();
    // per propstat: only props under a propstat whose <status> contains " 200 " are authoritative
    // collection iff <resourcetype><collection/></resourcetype> present
}
```
### Anti-Patterns to Avoid
- **Diffing percent-encoded hrefs against decoded config paths** — always decode hrefs once at parse time and work in decoded space; re-encode only when building request URLs.
- **Deleting state rows after a failed/partial walk** — a mid-walk error means an incomplete set; removals only after a complete walk (else one network blip logs every file as removed).
- **Re-enqueuing on weak-etag churn** — servers rotate `W/"…"` etags per request; that's exactly what the sha256 backstop (D-07) is for. Never enqueue on etag-diff alone without the sha256 gate for same-content files.
- **Treating the first `<d:response>` (the collection itself) as a file** — Depth:1 always includes self; skip it.
- **Logging per unchanged file** — success criterion 3's spirit: a no-op poll is silent (zero log.md entries, DEBUG-level tracing only).
- **Putting the poller's fs writes outside `vault::raw`/`vault::log`** — check.sh write-funnel guard will fail CI.
</architecture_patterns>

<dont_hand_roll>
## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| XML parsing of 207 bodies | string scanning / regex on XML | roxmltree 0.21 | Namespaces, entity encoding, attribute order, `<propstat>` grouping are exactly what regex gets wrong |
| HTTP + basic auth + redirects | raw TcpStream client for the poller | existing reqwest client | Auth header, redirect policy, TLS (rustls), timeouts, body streaming already solved |
| Retry/backoff on poll failure | in-poller retry loop | D-11's next-tick semantics + queue ladder for job work | Interval IS the retry loop; adding backon here duplicates it (CONTEXT D-11 forbids instant retries) |
| Durable enqueue | new notification table | `store.insert_job` | Phase 2 queue is complete (claim/backoff/dead-letter) |
| Vault writes | direct fs | `write_raw`/`write_raw_binary`/`append_entry` | PIPE-06 funnel + CI guard |
| Cron scheduling | tokio-cron-scheduler | `tokio::time::interval` | D-10 fixed intervals; HERMES.md stack note agrees |

**Key insight:** the *only* genuinely hand-rolled piece is ~150 lines of PROPFIND/sync-collection request/response shaping + multistatus parsing — the WebDAV client "library" question collapses to that once reqwest and roxmltree are chosen. The 0.3.3 reqwest-pin conflict makes any library route strictly worse here.
</dont_hand_roll>

<common_pitfalls>
## Common Pitfalls

### Pitfall 1: reqwest version duplication via transitive pin
**What goes wrong:** adding `reqwest_dav 0.3` silently pulls reqwest 0.13 next to 0.12; types stop matching, binary bloats, cargo-deny flags it, MoriNet alignment (QUAL-05) breaks.
**Why it happens:** crates.io "latest & maintained" looks correct in isolation.
**How to avoid:** check `crates.io/api/v1/crates/<name>/<ver>/dependencies` before adding any HTTP-adjacent crate; keep one reqwest in the tree.
**Warning signs:** `cargo tree -d` shows two reqwest majors.

### Pitfall 2: Restart re-scan from state written before enqueue
**What goes wrong:** poller upserts watch_state (etag/sha256) *before* the job completes; a crash between loses the ingestion forever (file looks unchanged next poll).
**Why it happens:** intuitive write order.
**How to avoid:** mirror Phase 3's 03-04 lesson (sha256 stored AFTER extraction succeeds): record the etag/size/sha256 row update only after `insert_job` returns (job is durable at that point — queue re-claim covers handler crash). Worst case after crash = one duplicate enqueue, which the sha256 no-op absorbs.
**Warning signs:** a file that vanished from the queue without a vault trace.

### Pitfall 3: Removal detection on partial walks
**What goes wrong:** one PROPFIND fails mid-walk; the diff sees "everything under the failed folder is gone" → mass log.md removal spam + state wipe → next walk re-ingests the whole tree.
**Why it happens:** diff logic assumes a complete universe.
**How to avoid:** compute removals only from a walk that completed all levels without error; on error, apply additions/changes only and set `last_error` (D-11).
**Warning signs:** dozens of `removal` log entries after a network blip.

### Pitfall 4: href relativization bugs (encoding + trailing slash)
**What goes wrong:** `%20`/UTF-8 filenames mismatch between walk and state; collection hrefs (`dir/`) vs file hrefs compared with off-by-one prefix logic; duplicate state rows for `a b.md` vs `a%20b.md`.
**Why it happens:** WebDAV hrefs are URI paths, not filesystem paths.
**How to avoid:** decode at the edge (parse), canonical decoded relative paths in the DB, `Url::join` for requests; unit-test with spaces, `é`, `&`, `#` in names.
**Warning signs:** same file enqueued every poll.

### Pitfall 5: sync-token staleness treated as fatal
**What goes wrong:** server expires sync tokens (allowed by RFC 6578 §3.1); a 400 on the stored token latches `sync_unsupported` or kills the watcher.
**How to avoid:** on failure WITH a non-empty token: retry once with empty token (full re-sync, cheap because etag/size gate prevents re-ingest); only latch unsupported on failure of the empty-token attempt.
**Warning signs:** `last_error` stuck on sync errors while PROPFIND would work.

### Pitfall 6: Secret leakage through URLs and errors
**What goes wrong:** credentials embedded in URL userinfo or echoed in `CoreError`/tracing when a request fails (reqwest error Display includes the URL — fine for path, fatal if creds were in it).
**Why it happens:** basic-auth convenience of `https://user:pass@host`.
**How to avoid:** creds ONLY via `.basic_auth()` from `SecretString::reveal()` at request-build time; never in the stored `source_id`/config url; error strings carry scheme+host+path only (D-02 secrecy).
**Warning signs:** `grep -i pass` over test log output; Debug of config structs in tracing.

### Pitfall 7: default_config / config.example.toml drift
**What goes wrong:** adding `[[watch]]` docs to one file fails `committed_example_config_matches_default_config` byte-identity test.
**How to avoid:** same-commit dual update (already a known hard constraint; put it as an explicit plan task).
</common_pitfalls>

<test_strategy>
## Test Strategy (Q5) — Stub Server Decision

**Decision: raw `std::net::TcpListener` stub in `crates/saussunet-core/tests/watch_webdav.rs`, following `tests/llm_provider_thinking.rs`.**

Guard analysis (verified against `scripts/check.sh`):
- axum guard is `grep -q 'axum' crates/saussunet-core/Cargo.toml` — matches ANY line including `[dev-dependencies]`. **axum as a core dev-dep is forbidden.** (A test-only axum PROPFIND responder is only viable in the *binary* crate's tests — wrong crate for core watch tests.)
- `wiremock` would pass the grep (name doesn't contain "axum"; it's hyper-based) and supports `Mock::given(method("PROPFIND"))` — a legitimate fallback, but it's a new dev-dep for something ~80 lines of std code does with more control (stateful file trees, per-item 207 bodies).
- write-funnel guard greps `crates/saussunet-core/src/` only — tests/ files may use `fs` freely (tempdir fixtures).
- zero-real-LLM guard greps two named test files only; new watch tests must still use `MockProvider` for any pipeline drain they do (structural D-14: construct MockProvider directly, no config/env).

**Stub design:** threaded accept-loop (one `std::thread`, `std::io::Read` per connection like the existing stub) serving from an `Arc<Mutex<HashMap<String /*decoded path*/, StubFile { bytes, etag }>>>` tree. Handles: `PROPFIND` (parse Depth + path; synthesize 207 with correct `resourcetype`/`getetag`/`getcontentlength`, collection-self first), `GET` (bytes + ETag header), `POST` (sync-collection: canned delta responses driven by a scripted queue, or 400 to exercise fallback). Tests mutate the map between polls: insert file → next poll enqueues; change bytes+etag → enqueues; remove → removal log + state row gone; unchanged → assert zero jobs + zero log.md delta.

**Deterministic new/changed/removed simulation:** tests call `poll_once(...)` directly (not the interval loop) — the interval loop itself gets one smoke test with a 10 ms interval + `tokio::time::sleep` + `insert_job` observation.

**Restart-resume proof (SRCC-02):** tempdir vault + **file-backed** SQLite (not `connect_memory`): boot Store → poll → assert 1 job → `drop(store)` → `Store::connect(same path)` → poll again → assert **zero new jobs** and `sync_state.last_poll_at` advanced. (In-memory SQLite would reset on drop and fake a pass.)

**Sync-token tests:** stub advertises sync on source A (207 with `<d:sync-token>`) and 400s on source B; assert A persists token in `sync_state.token` and second poll sends it (stub records request bodies via mpsc channel, exactly like the thinking-field test); assert B latches `sync_unsupported=1` and subsequent polls are PROPFIND-only.
</test_strategy>

<validation_architecture>
## Validation Architecture

**Test framework:** `cargo test` (tokio `#[tokio::test]` integration tests in `crates/saussunet-core/tests/`, unit tests in-module), gated by `scripts/check.sh` (fmt, clippy `-D warnings`, zero-real-LLM guard, tests with `SAUSSUNET_LLM__API_KEY` provably unset, axum-absence guard, write-funnel guard, release build). Every acceptance criterion below is a runnable command.

### Critical test cases (map 1:1 to success criteria)

| # | Criterion (ROADMAP) | Test | Type | How to Run |
|---|---------------------|------|------|-----------|
| T1 | SC1: new file detected → job enqueued | `watch_webdav::new_file_enqueues_ingest_job` — stub tree + poll_once → assert `list_jobs(pending)` has `ingest_raw` with correct payload; raw/ copy exists with frontmatter | integration | `cargo test -p saussunet-core --test watch_webdav` |
| T2 | SC1: changed file (etag+size differ) → re-enqueue | `changed_file_reenqueues` | integration | same |
| T3 | SC1: removed file → log.md entry + state row deleted, vault pages untouched | `removed_file_logs_and_prunes_state` | integration | same |
| T4 | SC1: Depth:1 walk respects depth cap; never sends `Depth: infinity` | `walk_depth_cap` — stub records every Depth header + path; assert all `Depth: 1`, no descent past cap | integration | same |
| T5 | SC1: poll interval + first-poll-on-startup | `interval_loop_polls_on_spawn_and_repeats` (10 ms interval) | integration | same |
| T6 | SC2: restart resumes with no re-scan | `restart_resumes_without_rescan` — file-backed SQLite, poll → drop → reopen → poll → zero new jobs | integration | same |
| T7 | SC2: etag/token/last-check persisted | `sync_state_updated_after_walk` (token, last_poll_at, last_error cleared) + `poll_failure_records_last_error` (D-11: WARN, non-fatal, next tick works) | integration | same |
| T8 | SC2/sync: token round-trip + fallback latch | `sync_token_persisted_and_reused`, `sync_unsupported_falls_back_forever` | integration | same |
| T9 | SC3: unchanged re-poll enqueues nothing, zero log noise | `unchanged_poll_is_noop` — assert jobs count unchanged AND log.md bytes unchanged | integration | same |
| T10 | SC3: weak etag churn → sha256 gate blocks enqueue | `weak_etag_same_bytes_no_enqueue` — stub rotates etag, same bytes → no job, row updated | integration | same |
| T11 | D-08: ext allowlist — .md/.txt → ingest_raw, .pdf → ingest_pdf, other skipped once | `extension_allowlist_routing`, `skipped_ext_logged_once_not_every_poll` | integration | same |
| T12 | D-02 secrecy: password absent from Debug of resolved config and from poller error strings | `watch_password_never_in_debug` (mirror of `debug_output_never_leaks_api_key`) | unit | `cargo test -p saussunet-core config` |
| T13 | Config: `[[watch]]` parses with defaults; bare/absent section OK; zero interval/depth rejected (WR-02 parity); example template byte-identical | `bare_watch_section_defaults`, `zero_interval_rejected`, existing `committed_example_config_matches_default_config` | unit | `cargo test -p saussunet-core config` + `lib` |
| T14 | multistatus parser edge cases: percent-encoded hrefs, collection-self, 404-propstat props, weak etags, per-item 404 in sync report | `multistatus::*` unit tests (pure, no HTTP) | unit | `cargo test -p saussunet-core watch` |
| T15 | `ingest_raw` handler end-to-end with MockProvider (zero real LLM) | extend existing pipeline test harness with `ingest_raw` kind | integration | `cargo test -p saussunet-core pipeline` |
| T16 | Migration 0009 applies cleanly on a Phase-3-era DB | covered by `sqlx::migrate!` on every `Store::connect` in existing tests | implicit | full `cargo test` |

### Coverage boundaries
- **Pure logic (diff, multistatus parse, href relativization, config resolve):** unit tests, no I/O — fast inner loop.
- **Protocol I/O:** stub-server integration tests only; no real WebDAV server in CI (LAN servers are operator-side; a manual UAT checklist item against a real server — e.g. Apache mod_dav or Nextcloud — belongs in the phase's UAT, not CI).
- **Non-goals for validation:** LLM output quality (Phase 3 owns), UI (Phase 5).
- **Guard compliance checklist per plan:** no `fs::write`/`File::create` outside `vault/atomic.rs`/`vault/raw.rs` in src; no `axum` string in core Cargo.toml; no `OpenAiCompat` in mock-only tests; `cargo fmt --check` + `cargo clippy --all-targets -- -D warnings` clean.

**One-command validation:** `bash scripts/check.sh` must pass with the new tests added — that is the phase gate.
</validation_architecture>

<open_questions>
## Open Questions

1. **sync-level choice (infinity-at-root vs per-collection level 1)**
   - Known: single `sync_state.token` per source fits D-05; `sync-level: infinity` at the root preserves that and is spec-legal (it is not PROPFIND Depth).
   - Unclear: whether the user reads SRCC-01's "never Depth: infinity" as extending to sync-level infinity in spirit.
   - Recommendation: ship infinity-at-root, document the distinction in the plan; if the user objects, per-collection tokens are a schema change — flag at plan review.

2. **Poller behavior under `mock` provider (WR-03 parity)**
   - Known: workers don't drain under mock; poller enqueueing anyway grows the pending queue durably.
   - Unclear: whether to suppress enqueue (watcher inert) or enqueue-and-wait under mock.
   - Recommendation: enqueue anyway (durable queue is the buffer; jobs drain the moment a real provider is configured); note as a one-line decision in the plan.

3. **Watch source identity / multiple entries with same URL**
   - Known: `source_id` keys sync_state/watch_state; config has no explicit id field yet.
   - Recommendation: derive `source_id` from the config `url` verbatim (matches Phase 3's URL-as-source_id convention); optionally allow an explicit `id = "..."` key to disambiguate two entries on one server. Planner picks; keep it stable across restarts (state key!).

4. **Include-globs (D-01 "optional include globs")**
   - Known: D-08's extension allowlist covers the stated file-handling need; globs add a `glob` dep + precedence rules.
   - Recommendation: ship extension allowlist only in v1; leave `include` key parsed-but-documented-as-reserved or defer entirely. Low risk either way.

5. **First-poll jitter across sources (CONTEXT discretion)**
   - Recommendation: trivial `tokio::time::sleep(fast-random 0..5s)` before first poll per source, or just stagger by spawn order; thundering herd is negligible at personal-LAN scale — don't over-engineer.
</open_questions>

<sources>
## Sources

### Primary (HIGH confidence)
- Live repo reads (2026-11-01): `store/sync_state.rs`, `store/jobs.rs` (insert_job:58), `queue/mod.rs`, `pipeline/mod.rs`, `config.rs`, `lib.rs` (default_config + byte-identity test), `vault/raw.rs`, `vault/log.rs` (append_entry:61), `crates/saussunet/src/main.rs` (boot), `scripts/check.sh` (exact guard greps), `migrations/` listing (0008 latest), root + core `Cargo.toml` (reqwest pinned 0.12; core dev-deps = tempfile only), `tests/llm_provider_thinking.rs` (stub pattern).
- RFC 6578 (rfc-editor.org/rfc/rfc6578.txt, fetched full): sync-collection marshalling (POST, Depth 0, sync-token/sync-level/prop/limit), 207 + trailing sync-token, 404-status = removed member, opaque-token rule, stale-token fallback, Appendix B client algorithm.
- crates.io API: `reqwest_dav` (0.3.3 published 2026-03-02, deps incl. `reqwest ^0.13`; 0.2.2 deps `reqwest ^0.12`; feature/lineage verified), `roxmltree 0.21.2` (2026-10-24, MIT OR Apache-2.0), `quick-xml 0.42.0` (edition 2024), `webdav-request 0.4.0` (6.4k downloads).
- docs.rs reqwest_dav 0.2.2: Client API surface (get/put/delete/mkcol/list; list_cmd PROPFIND types incl. ListResourceType) — **no REPORT/sync-collection method**.

### Secondary (MEDIUM confidence)
- HERMES.md stack research (2026-10-25): reqwest_dav Medium confidence, "manual PROPFIND with reqwest + quick-xml" named alternative; `dav-client` absent from crates.io (re-verified: not searched again, consistent with crates.io keyword count of 36 webdav crates).
- RFC 4918 semantics (PROPFIND Depth:1 includes self, multistatus propstat grouping, weak etags) — from spec knowledge, cross-checked against RFC 6578 Appendix B's client algorithm; no independent re-fetch of 4918 in this session.

### Tertiary (LOW confidence — validate during implementation)
- Real-world LAN server quirks (Nextcloud/Apache/nginx-davext sync support, etag formats) — first real-server UAT will tell; stub tests deliberately cover both sync-capable and 400-fallback shapes.
</sources>

<metadata>
## Metadata

**Research scope:**
- Core technology: hand-rolled WebDAV client (reqwest 0.12 + roxmltree) for PROPFIND Depth:1 walk + RFC 6578 sync-collection
- Ecosystem explored: reqwest_dav (both lines), webdav-request, dav-client (absent), quick-xml vs roxmltree, wiremock-as-stub viability
- Patterns: interval-task per source, try-and-fallback sync discovery, diff-and-swap state, sha256 backstop gate, write-after-enqueue ordering
- Pitfalls: dependency duplication, state-before-enqueue crash, partial-walk removal storms, href encoding, token staleness, secret leakage, config byte-identity coupling

**Confidence breakdown:**
- Standard stack: HIGH — crate versions/deps verified via crates.io API same-day; reqwest pin conflict is dispositive
- Architecture: HIGH — all integration signatures read from live code
- Pitfalls: HIGH (codebase-derived) / MEDIUM (server-quirk set, needs real-server UAT)
- Code examples: HIGH for reqwest/roxmltree shapes (official docs patterns); MEDIUM for exact stub-server ergonomics (will firm up in plan)

**Research date:** 2026-11-01
**Valid until:** 2026-12-01 (stable stack; re-check reqwest_dav/reqwest pins only if the workspace bumps reqwest to 0.13)
</metadata>

---

*Phase: 04-continuous-source-watching*
*Research completed: 2026-11-01*
*Ready for planning: yes*
