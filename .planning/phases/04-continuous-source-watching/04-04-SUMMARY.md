---
phase: 4
plan: 04
one_liner: "walking skeleton: DavClient (reqwest PROPFIND/GET, size caps, basic-auth hygiene) + poll_once (walk→diff→download→enqueue) + ingest_raw job kind + stateful raw-TcpListener WebDAV stub"
requirements_completed: [SRCC-01, SRCC-02]
---

# Plan 04-04 Summary — walking skeleton: poll a stub WebDAV server, enqueue a real job

## What was built

- `crates/saussunet-core/src/watch/dav.rs` (new) — `DavClient` (reqwest 0.12):
  - `new(&ResolvedWatch)` — 30 s timeout, redirects ≤5, base URL trailing-slash
    normalised (collection target), auth captured as
    `Option<(String, SecretString)>`.
  - `propfind_children(rel)` — Depth:1 PROPFIND, body verbatim from
    RESEARCH §protocol_notes, `Content-Type: application/xml`,
    `.basic_auth()` preemptive when creds configured; requires 207
    (`StatusCode::MULTI_STATUS`); body capped at
    `MAX_MULTISTATUS_BYTES = 8 MiB` BEFORE `parse_multistatus(xml,
    url.path())` (declared Content-Length refused up front, lying one caught
    chunk-by-chunk — fetch.rs pattern).
  - `get_bytes(rel, max_bytes)` — GET + basic auth, streaming size cap,
    non-success → `CoreError::Fetch`.
  - URL building: manual segment encoder (keep `[A-Za-z0-9._~-]`, `%XX` per
    UTF-8 byte) + `Url::join`. Error Display = `scheme://host/path` + reason
    only — reqwest errors classified via `is_connect()/is_timeout()` so the
    raw error Display (which embeds the URL) never rides along. No userinfo
    URL construction anywhere (`grep '://.*:.*@'` empty).
- `crates/saussunet-core/src/watch/mod.rs` — `pub mod dav;` + `poll_once`:
  `pub async fn poll_once(store: &Store, vault: &Arc<Vault>,
  src: &ResolvedWatch, max_fetch_bytes: u64) -> Result<usize>`.
  Flow: `propfind_children("")` → status-404 entries ignored (v1, sync 04-07)
  → `WalkEntry::from(&DavEntry)` → `list_watch_rows` → `StateEntry` →
  `diff_walk` → dir rows upserted (`is_dir=true`, etag/size NULL) →
  Added/Changed: `get_bytes` → sha256 → `.md`/`.txt` → `submit_raw`
  (source_hint = `{url}/{rel}`), `.pdf` → `submit_pdf` → **then**
  `upsert_watch_row` (explicit `// crash-safety (Pitfall 2): durable job
  first, state second` comment) → `record_sync_poll(vault_id, source_id,
  Some(now), None)` at success. Removed/Skipped deltas not acted on in v1
  (04-05). Never logs — the 04-06 loop owns WARN (D-11).
- `crates/saussunet-core/src/pipeline/mod.rs` —
  `JOB_KIND_INGEST_RAW = "ingest_raw"`, `IngestRawPayload` (same serde shape
  as `IngestPdfPayload`), `submit_raw(store, vault, bytes, filename,
  source_hint, max_bytes) -> Result<(i64, String)>` mirroring `submit_pdf`
  WITHOUT the `%PDF-` magic check: size cap → md|txt extension gate →
  `sanitize_component(stem)` → `write_raw` (text funnel, subdir `Some("watch")`,
  `RawFrontmatter{source_url: source_hint, ingested, sha256}`) → payload
  `{raw_path, source_id: raw_rel, filename}` → `insert_job`. Worker arm in
  `handle_job` dispatch → `ingest_raw`: same `raw/` path guard as
  `ingest_pdf`, sha256 of file bytes, `get_source_sha256` no-op skip,
  `set_source_sha256`, `strip_raw_frontmatter` (prompt sees body only), then
  `run_extraction_passes` exactly as the ingest_pdf tail.
- `crates/saussunet-core/tests/watch_webdav.rs` (new) — STATEFUL raw
  `std::net::TcpListener` stub (zero web framework): tree
  `Arc<Mutex<HashMap<String /*decoded rel, dirs end '/'*/, StubFile>>>`,
  request log `Arc<Mutex<ReqLog>>` where
  `type ReqLog = Vec<(String method, String path, Option<String> depth, String body)>`;
  per-connection threads; PROPFIND → 207 (self first, one-level children,
  resourcetype/getetag/getcontentlength per 04-03 fixtures, hrefs
  percent-encoded); GET → bytes + `ETag` header (404 if absent); POST → 400
  (D-05 fallback) with a `set_sync_mode(bool)` knob pre-wired for 04-07.
  `StubHandle`: `insert/remove/set_etag/requests/set_sync_mode`.
  Tests build `ResolvedWatch` through `toml::from_str::<FileConfig>` +
  `resolve()` (04-01 end-to-end).
- `crates/saussunet-core/tests/pipeline_raw.rs` (new) — T15 worker-drain
  harness (pipeline_pdf.rs pattern): `submit_raw` → `spawn_workers` with
  MockProvider (pass1 + page JSON citing the raw/watch rel) → job `done` →
  `concepts/signifier.md` on disk with `^[raw/watch/…-note.md]` provenance.

## Tests added (all TDD, RED observed before GREEN)

Task 1 (RED: E0432 compile-absence of `watch::dav`):
`propfind_lists_children`, `get_bytes_respects_size_cap`,
`error_string_has_no_password` (closed port + sentinel password; Display
carries neither the secret nor `Basic`), `href_with_spaces_roundtrips`
(`a b.md` propfind path → get_bytes succeeds — segment encoding proven).

Task 2 (RED: E0432/E0425 compile-absence of `JOB_KIND_INGEST_RAW`,
`poll_once`, `pipeline::submit_raw`):
`new_file_enqueues_ingest_job` (T1: 2 ingest_raw jobs, `raw/watch/` copies
with `source_url:`/`sha256:` frontmatter on disk, second poll enqueues 0,
watch_state rows for both files + `papers` dir row, `record_sync_poll`
landed), `extension_allowlist_routing` (T11a: a.md+b.txt → 2 ingest_raw,
c.pdf (`%PDF-` magic) → 1 ingest_pdf, d.docx → no job),
`ingest_raw_end_to_end` (T15, MockProvider — zero real LLM).

Final: `cargo test -p saussunet-core` → **245 passed, 0 failed**
(baseline 238 + 7 new). `bash scripts/check.sh` → check: OK (fmt, clippy
`-D warnings`, zero-real-LLM, axum-absence, write-funnel, release build).

## Deviations

- `crates/saussunet-core/tests/security_hygiene.rs` (not in plan
  files_modified): `no_secrets_in_log_paths` confined `reveal()` to
  `llm/openai_compat.rs`; the plan's threat model mandates a second
  sanctioned call site (DavClient basic-auth). Allowlist extended to
  `watch/dav.rs` with a non-vacuity assertion mirroring the existing one.
- Stub file comments avoid the literal word the axum-absence acceptance grep
  searches for (`grep -c 'axum' watch_webdav.rs` == 0 as required).
- One intermediate full-suite run showed a single queue-test failure
  (known SQLITE_BUSY flake family); two subsequent full runs and check.sh
  were fully green. Not reproduced, not debugged (per executor rules).

## Decisions

- `DavClient::new` normalises the base URL to a trailing slash so `Url::join`
  keeps the root collection segment instead of replacing its last one.
- reqwest transport errors are classified (`connection failed` / `timed out` /
  `request failed`) instead of interpolating reqwest's Display (which embeds
  the request URL) — keeps error strings strictly scheme+host+path+reason.
- `ingest_raw` worker strips the D-07 frontmatter block before the pass-1
  prompt (`strip_raw_frontmatter`); the sha256 digest is of the FULL file
  bytes (frontmatter included) — stable for the re-queue no-op defense.
- `poll_once` upserts dir rows from the walk (not from deltas — `diff_walk`
  emits none for dirs) with etag/size NULL per plan.
- Non-routable allowlist extensions (custom configs, e.g. `docx`): no job and
  no state row in v1 → re-downloaded next poll; routing/skip-logging is 04-05
  territory.

## Lessons

- The security audit test greps `src/` for `reveal()` — any new sanctioned
  secret call site must update `security_hygiene.rs` in the SAME commit or
  check.sh fails late.
- clippy `-D warnings` fires `type_complexity` on 4-tuple request logs —
  define a `type ReqLog` alias up front.
- The stub must percent-encode child href segments (spaces → `%20`) and keep
  the trailing `/` OUT of the encoder for directories, or the 04-03 parser
  produces `%2F`-mangled collection paths.
- `spawn_workers` coerces `Arc<MockProvider>` → `Arc<dyn LlmProvider>` at the
  call site; importing `LlmProvider` in the test is an unused-import warning
  (clippy -D warnings fails on it).
