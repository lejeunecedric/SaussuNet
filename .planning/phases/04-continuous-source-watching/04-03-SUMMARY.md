---
phase: 4
plan: 03
one_liner: "pure watch logic: roxmltree multistatus/sync-report parser (DavEntry/parse_multistatus/extract_sync_token) + pure diff_walk classifier (WalkEntry/StateEntry/WatchDelta)"
requirements_completed: [SRCC-01, SRCC-02]
---

# Plan 04-03 Summary — pure watch logic: multistatus parser + diff_walk (no I/O)

## What was built

- `Cargo.toml` + `crates/saussunet-core/Cargo.toml` — `roxmltree = "0.21"`
  (resolved 0.21.2) single-sourced in `[workspace.dependencies]`, added to
  core per the RESEARCH stack decision. No axum; no new reqwest major.
- `crates/saussunet-core/src/error.rs` — new `CoreError::Dav(String)` variant
  (short parse description only, never the response body — D-03 display
  safety).
- `crates/saussunet-core/src/lib.rs` — `pub mod watch;`.
- `crates/saussunet-core/src/watch/mod.rs` — `pub mod diff; pub mod
  multistatus;` (04-04 adds `pub mod dav;` + `poll_once` here).
- `crates/saussunet-core/src/watch/multistatus.rs` (new) — pure parser:
  - `pub struct DavEntry { path, etag: Option<String>, size: Option<i64>,
    is_collection: bool, status: u16 }`
  - `pub fn parse_multistatus(xml: &str, root_path: &str) -> Result<Vec<DavEntry>>`
    — namespace-aware (`tag_name().namespace() == Some("DAV:")`); props trusted
    only under a propstat whose `<status>` contains `" 200 "`; getetag verbatim
    (weak `W/"…"` preserved), getcontentlength → i64, resourcetype
    `<collection/>` OR trailing-slash href → collection; collection-self
    (href == root) skipped; per-item direct-child `<d:status>` parsed for sync
    reports (404 = removed marker, absent = 200). Hrefs percent-decoded ONCE
    (dependency-free byte-level `percent_decode`, '+' literal, lossy-UTF-8
    floor), relativized against the decoded root; crafted traversal hrefs stay
    relative opaque keys, never absolute paths (threat model).
  - `pub fn extract_sync_token(xml: &str) -> Option<String>` — trailing
    `<d:sync-token>` text, trimmed, empty→None.
- `crates/saussunet-core/src/watch/diff.rs` (new) — pure classifier, zero I/O
  imports (`grep -cE 'reqwest|sqlx|fs::' diff.rs` == 0):
  - `WalkEntry { path, etag, size, is_dir }`, `StateEntry { + sha256 }`,
    `WatchDelta::{Added,Changed}(WalkEntry) | Removed(StateEntry) |
    Skipped(WalkEntry)` (all `Debug + Clone + PartialEq + Eq`).
  - `impl From<&DavEntry> for WalkEntry` (is_collection → is_dir).
  - `pub fn diff_walk(walk: &[WalkEntry], state: &[StateEntry],
    allowlist: &[String]) -> Vec<WatchDelta>` — no state row → Added (ext ∉
    allowlist → Skipped first); any etag difference (incl. weak rotation) →
    Changed (both etags None → size decides); equal → nothing; state path
    absent from walk → Removed (dirs included — bookkeeping cleanup is the
    poller's); is_dir walk entries → no delta. Deterministic order:
    walk-pass deltas interleaved in walk order, then Removed in state order.
    `ext_matches` = file-name `rsplit_once('.')`, lowercased, vs the
    dotless-lowercase allowlist (04-01 normalization); no dot → no match.

## Tests added (all TDD, RED observed before GREEN)

Task 1 (RED: E0432 compile-absence of `DavEntry`/`parse_multistatus`/
`extract_sync_token`): `parses_plain_propfind`,
`decodes_percent_hrefs_and_relativizes`, `ignores_props_under_404_propstat`,
`weak_etag_preserved_verbatim`, `sync_report_marks_removed_items`,
`extracts_sync_token`, `trailing_slash_fallback_collection_detection`,
`traversal_href_stays_relative` — 8 tests over embedded PROP207/SYNC207
fixtures (04-04: reuse these strings verbatim as stub-server canned bodies).

Task 2 (RED: E0432 compile-absence of `diff_walk`/`WalkEntry`/`StateEntry`/
`WatchDelta`): `added_when_path_absent_from_state`,
`changed_on_etag_mismatch_even_weak`, `unchanged_when_etag_and_size_equal`,
`removed_when_in_state_absent_from_walk`,
`skipped_for_extension_outside_allowlist`, `dirs_never_enqueued`,
`extension_compare_is_case_insensitive_and_dotless`,
`removed_dir_children_each_removed` — 8 tests.

Final: `cargo test -p saussunet-core` → **238 passed, 0 failed**
(baseline 222 + 16 new). `bash scripts/check.sh` → check: OK.

## Deviations

- None on API or behavior. Two notes:
  - `cargo tree -d | grep reqwest` shows reqwest 0.11.27 (via readability 0.3)
    alongside the workspace 0.12 — PRE-EXISTING from Phase 3, not introduced
    here; the plan's criterion targets reqwest_dav's third major, which never
    entered the tree.
  - First `check.sh` run hit the known flake
    `unknown_kind_dead_letters_with_error_retained` (SQLITE_BUSY, documented
    in 04 executor notes); immediate re-run → check: OK.

## Decisions

- roxmltree 0.21 API: node namespace via `tag_name().namespace()` (there is
  no `namespace_uri()` method); `Node` is a Copy value type — helpers take it
  by value.
- `relativize` compares in slash-trimmed space (root arrives `/a/b/`, hrefs
  `/a/b/c`): strip `root + "/"` prefix, else full path minus slashes —
  non-matching hrefs become relative opaque keys (never absolute).
- Changed rule implemented exactly per plan GREEN spec: `etag differs →
  Changed` (Some-vs-None counts as differs); `both None → size decides`;
  equal Some etags → unchanged regardless of size. sha256 gate stays in the
  poller (D-07).
- Removed includes dir state rows absent from the walk (poller decides
  bookkeeping); walk-side dirs emit no delta.
- `CoreError::Dav(String)` added (plan's read_first allowed it; no existing
  variant fit parse failures).

## Lessons for 04-04 (walking skeleton)

- `poll_once` seam: `dav.rs` bytes → `parse_multistatus(body, url.path())` →
  `.map(WalkEntry::from)` → `diff_walk(&walk, &state_rows_as_StateEntry,
  &watch.extensions)` → deltas. Map `WatchRow` → `StateEntry` in the poller
  (diff.rs deliberately has no store import).
- Sync-report 404 items arrive as `DavEntry { status: 404 }` — translate to
  Removed handling at poller level (diff_walk is status-agnostic by design).
- Enforce the response size cap BEFORE parse (blocking threat control,
  recorded in the plan threat model); roxmltree itself is bomb-inert.
- The PROP207/SYNC207 fixture constants in multistatus.rs tests are the
  canned 207 bodies for the stub server — copy, don't re-invent.
