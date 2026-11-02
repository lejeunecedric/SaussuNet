---
phase: 4
date: 2026-11-02
range: 8556eaf..HEAD
verdict: issues_found
counts:
  critical: 0
  high: 1
  medium: 2
  low: 6
---

# Phase 4 Code Review — Continuous Source Watching

Reviewed: `crates/saussunet-core/src/watch/{mod,dav,multistatus,diff}.rs`,
`store/{watch_state,sync_state}.rs`, `pipeline/mod.rs` (ingest_raw arm),
`config.rs` ([[watch]] resolve), `crates/saussunet/src/main.rs`,
`migrations/0009_watch_state.sql`, `config.example.toml`, Cargo diffs.
Cross-checked against 04-01..04-07 PLAN threat models, 04-VALIDATION.md, and
all seven SUMMARY deviation notes.

**Overall:** credential hygiene is genuinely solid (SecretString masked Debug,
basic-auth header-only with sanctioned `reveal()` call sites, sanitized
scheme+host+path error surface, newline-stripped log/last_error lines,
userinfo-URL rejection at resolve). Store layer is fully parameter-bound with
narrow per-column writers — no injection sinks, no all-column clobber on the
poll path. XML safety (roxmltree no-DTD + 8 MiB pre-parse cap + streaming
download cap) matches the threat models. Crash-safe ordering (insert_job →
row upsert), complete-walk-only removals, sync-delta ≠ universe (no diff-based
removals), and token-advances-after-apply are all correctly implemented.
The findings below are one real availability/integrity bug class, two
trust-boundary/latch gaps, and minor robustness items.

## Critical

None.

## High

### H1 — A single failing file aborts the whole poll: permanent log.md loss + source stall
`crates/saussunet-core/src/watch/mod.rs:197` (walk loop) and `:391` (sync loop)
propagate any per-file error from `apply_file_delta` via `?`. Consequences:

1. **Permanent log loss.** `lines` is flushed to log.md only after the delta
   loop (`mod.rs:243-246` / `:434-437`). If file *N* errors, the accumulated
   `added:`/`changed:` lines for files 1..N-1 are dropped — but their
   `watch_state` rows were already upserted with the new etag+sha256, so the
   next poll sees no delta and those log entries are **never written**. Same
   on the sync path: the token does not advance, the delta replays, but the
   sha256 gate turns replays into silent row updates — log lines lost forever.
2. **Source stall on a persistently failing file.** A file that always fails
   (GET 403/404 race that never resolves, file larger than `max_fetch_bytes`,
   or a dotfile named `.md`/`.txt` — `pipeline/mod.rs:247` slices an empty
   stem and `sanitize_component("")` refuses it) aborts the loop at the same
   position every tick. Its state row is never written, so it re-fails forever,
   and any new file sorting **after** it in walk order is never ingested. The
   source is silently wedged (WARN-only loop, D-11).

The 04-05 SUMMARY records the abort-on-download-error choice as deliberate,
but the two consequences above are not acknowledged there.

**Fix direction:** catch per-file errors inside the delta loop: record the
failure (last_error / a skip note), continue with remaining deltas, and flush
`lines` before returning the poll-level Err. For the dotfile edge, reject
empty stems as a Skipped note in `apply_file_delta` rather than a fatal error.

## Medium

### M1 — Dot-segment hrefs escape the watched root when request URLs are rebuilt
`watch/multistatus.rs:173-186` deliberately keeps a crafted `../../etc/passwd`
href as a "relative opaque key" (its own test asserts this), but
`watch/dav.rs:213-225` (`url_for`) rebuilds request URLs from that key with
`Url::join`, and `encode_segment` (`dav.rs:253-263`) keeps `.` — so `..`
segments survive and RFC 3986 dot-segment removal resolves them **above** the
watch root (verified: base `http://h/dav/vault-inbox/` + rel `../../etc/passwd`
→ `http://h/etc/passwd`). A malicious/compromised server can thus steer GETs
and (via a collection href, `mod.rs:123-128`) PROPFINDs to arbitrary paths on
the same host — outside the folder the operator scoped. Raw *writes* remain
safe (slug folding), and D-03 declares the server trusted, so this is a
defense-in-depth gap versus the plans' traversal threat model rather than an
open SSRF.

**Fix direction:** in `url_for`, reject any `rel` whose segments contain `.`
or `..` (or have `relativize` drop dot-segments when building the key).

### M2 — `sync_unsupported` latches on transient errors, permanently
`watch/mod.rs:303-311`: on the first poll (no stored token) **any** error —
connection refused, timeout, 5xx — latches `sync_unsupported` forever
(`:290-300` does the same after a stored-token retry). The plan's literal
wording ("failure of the empty-token attempt ⇒ latch") is followed, but D-05's
intent is "unsupported server latches"; a sync-capable server that is briefly
down at exactly the wrong moment is demoted to PROPFIND-walk-forever with no
un-latch path short of a manual DB edit.

**Fix direction:** latch only on HTTP-level "not supported" signals (400/405/501
or a non-207 status response); treat transport errors (`err_kind` ==
connection failed / timed out) as a poll failure without latching.

## Low

### L1 — `display_target` drops the port
`watch/dav.rs:230-237` formats `scheme://host/path` from `url.host_str()`,
which excludes the port — errors for a non-default-port server point at the
wrong target. Include `url.port()`.

### L2 — `propstat_is_ok` requires a trailing space after 200
`watch/multistatus.rs:131-137` matches `" 200 "`; a status line without a
reason phrase (`HTTP/1.1 200`) discards otherwise-valid props → the file
reads as Changed every poll (sha256 gate absorbs it, but it costs a GET).
Parse the code like `parse_status_code` instead.

### L3 — Interval catch-up contradicts the plan comment
`watch/mod.rs:565` uses the default `MissedTickBehavior::Burst`: after a poll
longer than the interval, missed ticks fire back-to-back — the 04-06 threat
model claims "missed ticks are skipped rather than queueing". Set
`tick.set_missed_tick_behavior(MissedTickBehavior::Delay)`.

### L4 — Duplicate walk paths double-enqueue
`watch/mod.rs:181-199`: `prior_sha` is read from the pre-loop `state`
snapshot, so a server bug (or sync report) listing the same path twice emits
two Added deltas and enqueues the same bytes twice. Dedupe walk entries by
path before the delta loop.

### L5 — `replace_watch_rows` is dead code
`store/watch_state.rs:91-122` is never called outside its own tests (the
poller uses per-row upsert + delete). Either wire it in or remove it to avoid
a second, transaction-based write path drifting from the live one.

### L6 — Sync server that omits `<d:sync-token>` forces full re-sync each poll
`watch/mod.rs:317-319` stores `None`, so every subsequent poll sends an
empty-token full report (cheap via the sha256 gate, but O(tree) GETs).
Consider treating "207 without token" as sync-unsupported or keeping the
prior token.

## Verified clean (spot-checked against the threat models)

- Credentials: `SecretString` masked Debug (T12), header-only basic auth,
  userinfo URLs rejected at resolve (`config.rs:389-394`), error strings
  carry scheme+host+path + static reason only, `last_error` newline-sanitized,
  security_hygiene allowlist extended with a non-vacuity assertion.
- Size caps: 8 MiB multistatus cap enforced pre-parse and chunk-wise
  (`read_capped`), per-file download cap streamed, sync token capped at
  4096 chars before persisting, token XML-escaped into the report body.
- Removal integrity: `walk_complete` gate (`mod.rs:227`), sync path ignores
  diff-based Removed with the mandated comment, 404-items only; vault pages
  never touched (D-09).
- Crash-safety ordering: `insert_job` → `upsert_watch_row` (Pitfall 2); token
  advances only after full delta application.
- Store: all queries parameter-bound; narrow writers
  (`record_sync_poll`/`set_sync_token`/`set_sync_unsupported`) preserve other
  columns; migration 0009 PK matches the diff key.
- Config: interval/depth zero-value rejection prevents hot-loop and
  `tokio::time::interval(0)` panic; duplicate-URL rejection matches the
  source_id key; example template byte-identical to the `lib.rs` twin.
- Boot wiring: one task per [[watch]], all handles aborted on ctrl_c
  (`main.rs`); no task leaks on the shutdown path.
- Cargo.lock: only `roxmltree` added (expected).
