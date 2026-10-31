# Phase 4: Continuous Source Watching - Discussion Log

**Date:** 2026-10-31
**Mode:** default (interactive) — timed out, defaults applied

## Areas Presented

1. **Source configuration backing store** — config.toml `[[watch]]` vs SQLite table + seed vs Claude decides
2. **Change-detection strategy** — etag-first per-file vs download+sha256 gate vs RFC 6578 sync-token
3. **Removed-file policy** — record only vs flag orphaned pages vs mirror delete
4. **Poller execution model** — in-daemon interval task vs watch-as-job in queue vs Claude decides

## Outcome

The `clarify` form timed out with no user responses. Claude-recommended defaults
were applied for all four areas and recorded in `04-CONTEXT.md` with an explicit
note that they are defaults, not user-locked decisions:

- D-01..D-03: config.toml `[[watch]]`, basic auth via SecretString, operator URLs trusted (SSRF guard not applied)
- D-04..D-07: etag-first per-file Depth:1 walk with depth cap, opportunistic RFC 6578 sync-token into `sync_state.token`, per-file state in SQLite, sha256 no-op as final gate
- D-08..D-09: extension allowlist (.md/.txt → new `ingest_raw` kind, .pdf → existing path), removals recorded only — pages never deleted
- D-10..D-12: in-daemon per-source `tokio::time::interval` task, failures recorded in `sync_state.last_error` and retried next tick, watcher tied to daemon lifecycle

## User Selections

None — no responses received before timeout.

## Deferred Ideas Captured

- Drive/MCP watchers (v2), Phase 5 UI for watcher management, orphaned-page flagging, rename detection.

## Claude's Discretion Items

PROPFIND client crate choice, per-file state table shape, `ingest_raw` payload schema, config key names/jitter, stub WebDAV test server approach.
