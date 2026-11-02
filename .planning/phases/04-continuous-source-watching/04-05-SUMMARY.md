---
phase: 4
plan: 05
one_liner: "poll_once production-honest: depth-capped BFS (Depth:1 only), complete-walk-only removals, sha256 backstop gate, sanitized last_error bookkeeping, fully silent no-op polls"
requirements_completed: [SRCC-01, SRCC-02]
---

# Plan 04-05 Summary — full walk semantics: depth cap, removals, sha256 gate, quiet failures

## What was built

- `crates/saussunet-core/src/watch/mod.rs` — `poll_once` split into a thin
  bookkeeping shell + `poll_inner`:
  - **BFS walk (D-04/SRCC-01)**: `VecDeque<(rel, depth)>` starting at
    `("", 0)`; each collection gets exactly one `propfind_children(rel)`
    (the ONLY request primitive — always `Depth: 1`, never infinity). A
    collection child is queued **only while `depth < depth_cap`** (root =
    depth 0; cap 4 ⇒ PROPFINDs at depths 0..4). Defensive `visited`
    HashSet on collection rel paths guards cyclic hrefs. Parser paths are
    collection-relative, so BFS prefixes `rel` to build root-relative
    paths. `walk_complete = walk_err.is_none()` — a failed PROPFIND keeps
    everything collected so far (partial walk), records the first error
    string, and continues with siblings.
  - **Diff/apply**: dir rows upserted from the walk (unchanged).
    Added/Changed: `get_bytes` → sha256 → **D-07 gate**: equal to the
    pre-walk `StateEntry.sha256` ⇒ row upsert with fresh etag/size, NO job,
    NO log line (weak-etag churn absorbed). Unequal ⇒ existing
    submit_raw/submit_pdf path, then row upsert AFTER insert_job returned
    (04-04 crash-safety order untouched).
  - **Removal pass (Pitfall 3 gate)**: `WatchDelta::Removed` arms begin with
    `if !walk_complete { continue; }` — the single most important line.
    File removals: `delete_watch_row` + `- removed: <path>` log line;
    removed dirs: row deleted silently. Vault pages never touched (asserted
    by byte-snapshot test, D-09).
  - **Skip notes (D-08)**: `Skipped` logs `- skipped (extension not
    allowlisted): <path>` ONLY when no prior state row exists; the marker
    row (etag/size, sha256 NULL) is the already-noted memory.
  - **log.md discipline**: paths newline-sanitized (`sanitize_line`), ONE
    `vault::log::append_entry(vault, "## [date] update | Watch", &lines)`
    call per poll, only when `lines` non-empty — a no-op poll writes zero
    bytes (T9).
  - **last_error (D-11)**: success ⇒ `record_sync_poll(Some(now), None)`;
    any Err (partial walk or download/submit failure) ⇒
    `record_sync_poll(Some(now), Some(sanitize_line(err)))` then Err.
    DavClient errors are already scheme+host+path sanitized (D-02);
    newline-stripping adds log/state-line injection defense. `poll_once`
    itself logs at DEBUG only (grep proves no warn!/error!) — the 04-06
    loop owns WARN. Partial-walk rule: additions/changes from the partial
    set ARE applied, removals skipped, Err returned so the loop WARNs.
- `crates/saussunet-core/tests/watch_webdav.rs` — stub upgrades: implicit
  collection synthesis (a deeper key `a/b/c.md` makes `a/` and `a/b/`
  walkable — PROPFIND children now a BTreeSet union of explicit keys and
  first-segment dirs), one-shot `fail_next_propfind()` knob (500 answer),
  500 reason line; test helpers `setup_watch_with(base, depth_cap, user,
  pass)` and `snapshot_dir` (byte-snapshot for the D-09 assertion).

## Tests added (9; all RED observed before GREEN)

Task 1 (RED against 04-04 parent — all 4 failed):
`walk_depth_cap` (T4: every logged PROPFIND has Depth Some("1"), no path
deeper than cap levels, deep file invisible at cap=2 and enqueued by a
second source at cap=4), `removed_file_logs_and_prunes_state` (T3: log
`removed: gone.md`, row pruned, pages byte-identical, zero jobs),
`skipped_ext_logged_once_not_every_poll` (T11b: exactly one skip line
across two polls, sha256-NULL marker row), `log_injection_via_filename_is_neutralized`
(threat model: `evil\nINJECTED.md` yields no forged `## [` header).

Task 2 (RED evidence taken by running the new tests against the 04-04
parent `watch/mod.rs`: `changed_file_reenqueues`,
`weak_etag_same_bytes_no_enqueue` and `poll_failure_records_last_error`
all FAILED there — no sha gate, no last_error, no log lines;
`unchanged_poll_is_noop` and `sync_state_updated_after_walk` were already
satisfied by the parent and serve as regression guards):
`changed_file_reenqueues` (T2), `weak_etag_same_bytes_no_enqueue` (T10:
row etag updated, sha256 unchanged, zero jobs), `unchanged_poll_is_noop`
(T9: log.md byte-identical between polls), `sync_state_updated_after_walk`
(T7a: last_poll_at strictly advanced, last_error None),
`poll_failure_records_last_error` (T7b: forced 500 ⇒ Err, last_error Some
and free of the sentinel password, zero rows deleted, log.md untouched,
next poll Ok + last_error cleared).

Final: `cargo test -p saussunet-core` → **254 passed, 0 failed**
(baseline 245 + 9 new). `bash scripts/check.sh` → check: OK.

## Deviations

- Task boundaries blurred: the BFS rewrite (Task 1) also carried the sha256
  gate and last_error shell (Task 2 scope) because they live in the same
  control flow; Task 2's RED evidence was therefore produced by running
  its tests against the 04-04 parent implementation (3/5 genuinely RED,
  2 regression guards — listed above).
- Plan Context line 44 showed `removed: <path> (<source_id>)` while the
  line-format list (line 45) says `removed: <path>`; followed the explicit
  format list (source is already implied by the Watch entry context).
- `now` switched from `to_rfc3339()` to
  `to_rfc3339_opts(SecondsFormat::AutoSi, true)` — same output in practice
  (chrono AutoSi is the to_rfc3339 default) but makes sub-second precision
  explicit so T7a's strict "advanced" assertion is meaningful.
- Download/submit errors (as opposed to walk errors) still abort the poll
  via `?` after recording last_error — the plan only mandates partial
  application for WALK errors; per-file abort-and-continue would risk
  retry-ordering subtleties with no test demanding it.

## Decisions

- Removed-delta gate lives inside the delta loop (`if !walk_complete {
  continue; }`) rather than filtering the delta vector — the reviewer's
  one-line check sits exactly where removals happen.
- Skipped marker rows carry etag/size so a later byte change re-emits
  Skipped (silently — the marker already exists), keeping the once-only
  log rule honest without re-downloading.
- `last_error` is sanitized with the same newline guard as log lines;
  DavClient/Fetch Display strings were already credential-free (04-04),
  the guard is defense-in-depth for any future error source.
- BFS `visited` set dedupes collection rels defensively (cyclic hrefs);
  files may legitimately appear once per parent listing so they are not
  deduped.

## Lessons

- `propfind_children(rel)` returns paths RELATIVE to the walked collection
  (the parser relativizes hrefs against the request URL path) — BFS must
  prefix `rel`; forgetting this silently flattens nested trees.
- Stub PROPFIND synthesis must derive implicit collections from deeper
  keys (first-segment union), and the file-entry `guard.get(...)` lookup
  must move into the file branch or implicit dirs panic it.
- clippy `-D warnings`: `depth + 1 <= cap` trips `int_plus_one` (write
  `depth < cap`); chained `.replace()` trips `collapsible_str_replace`
  (use the array pattern `replace(['\r','\n'], " ")`).
- When a rewrite front-loads a later task's behavior, honest RED is still
  obtainable: `git show <parent>:file` into the tree, run the new tests,
  record failures, restore.
