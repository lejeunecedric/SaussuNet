---
phase: 4
slug: continuous-source-watching
status: verified
nyquist_compliant: true
wave_0_complete: true
created: 2026-11-01
---

# Phase 4 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.
> Derived from `04-RESEARCH.md` §Validation Architecture (T1–T16).

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | `cargo test` (tokio `#[tokio::test]` integration tests in `crates/saussunet-core/tests/`, in-module unit tests), gated by `scripts/check.sh` |
| **Config file** | none — existing workspace infrastructure |
| **Quick run command** | `cargo test -p saussunet-core --test watch_webdav` (plus touched suite) |
| **Full suite command** | `bash scripts/check.sh` |
| **Estimated runtime** | quick ~10 s · full ~120 s |

---

## Sampling Rate

- **After every task commit:** `cargo test -p saussunet-core --test watch_webdav` (or the suite the task touched)
- **After every plan wave:** `bash scripts/check.sh`
- **Before `/gsd-verify-work 4`:** Full suite must be green
- **Max feedback latency:** ~120 s

---

## Per-Task Verification Map

To be filled from plan `<acceptance_criteria>` when plans land (planner maps T1–T16 from `04-RESEARCH.md` §Validation Architecture 1:1 to tasks).

| Task ID | Plan | Wave | Requirement | Threat Ref | Secure Behavior | Test Type | Automated Command | File Exists | Status |
|---------|------|------|-------------|------------|-----------------|-----------|-------------------|-------------|--------|
| 04-01/1 | 04-01 | 1 | SRCC-01 | creds-in-URL rejection | SecretString password; userinfo URLs rejected | unit (tdd) | `cargo test -p saussunet-core config` | ❌ W0 | ⬜ pending |
| 04-01/2 | 04-01 | 1 | SRCC-01 | TM 04-01 | T12 debug secrecy + T13 validation + example byte-identity | unit (tdd) | `cargo test -p saussunet-core config` | ❌ W0 | ⬜ pending |
| 04-02/1 | 04-02 | 1 | SRCC-02 | — (schema only) | parameter-bound sqlx; no creds in schema | integration | `cargo test -p saussunet-core store` | ❌ W0 | ⬜ pending |
| 04-02/2 | 04-02 | 1 | SRCC-02 | — | no all-column clobber (narrow upserts) | unit | `cargo test -p saussunet-core store` | ❌ W0 | ⬜ pending |
| 04-03/1 | 04-03 | 2 | SRCC-01 | XML bomb (roxmltree no-DTD) | T14 parser edge cases | unit (tdd) | `cargo test -p saussunet-core watch` | ❌ W0 | ⬜ pending |
| 04-03/2 | 04-03 | 2 | SRCC-01/02 | — (pure) | diff purity (no I/O imports) | unit (tdd) | `cargo test -p saussunet-core watch` | ❌ W0 | ⬜ pending |
| 04-04/1 | 04-04 | 3 | SRCC-01 | HIGH: creds in errors/URLs; size caps | basic_auth only; 8 MiB cap pre-parse | integration | `cargo test -p saussunet-core --test watch_webdav` | ❌ W0 | ⬜ pending |
| 04-04/2 | 04-04 | 3 | SRCC-01/02 | HIGH: traversal via hrefs → sanitizer | T1/T11a/T15; crash-safe write order | integration | `cargo test -p saussunet-core --test watch_webdav && cargo test -p saussunet-core --test pipeline_raw` | ❌ W0 | ⬜ pending |
| 04-05/1 | 04-05 | 4 | SRCC-01 | HIGH: partial-walk removal storm; log injection | T3/T4/T11b | integration | `cargo test -p saussunet-core --test watch_webdav` | ❌ W0 | ⬜ pending |
| 04-05/2 | 04-05 | 4 | SRCC-02 | HIGH: last_error hygiene | T2/T7/T9/T10 | integration | `cargo test -p saussunet-core --test watch_webdav` | ❌ W0 | ⬜ pending |
| 04-06/1 | 04-06 | 5 | SRCC-01 | WARN-only failures | T5 interval loop | integration | `cargo test -p saussunet-core --test watch_webdav interval_loop` | ❌ W0 | ⬜ pending |
| 04-06/2 | 04-06 | 5 | SRCC-02 | — | T6 restart no re-scan (file-backed SQLite) | integration | `cargo test -p saussunet-core --test watch_webdav restart` | ❌ W0 | ⬜ pending |
| 04-07/1 | 04-07 | 6 | SRCC-02 | HIGH: opaque token cap; shared error helper | sync_collection + stale-retry/latch | unit/build | `cargo build -p saussunet-core` | ❌ W0 | ⬜ pending |
| 04-07/2 | 04-07 | 6 | SRCC-02 | — | T8 token round-trip + fallback latch + stale-token | integration | `cargo test -p saussunet-core --test watch_webdav` | ❌ W0 | ⬜ pending |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

**Critical case inventory (from RESEARCH.md):** T1 new-file enqueue · T2 changed-file re-enqueue · T3 removal log+prune · T4 depth-cap/never Depth:∞ · T5 interval+first-poll · T6 restart no re-scan (file-backed SQLite) · T7 sync_state persistence + failure last_error · T8 sync-token round-trip + fallback latch · T9 unchanged poll zero jobs/zero log noise · T10 weak-etag sha256 gate · T11 extension allowlist + skip-logged-once · T12 password never in Debug/errors · T13 `[[watch]]` config defaults/validation + example byte-identical · T14 multistatus parser edge cases · T15 `ingest_raw` handler with MockProvider · T16 migration 0009 applies cleanly.

---

## Wave 0 Requirements

- [ ] `crates/saussunet-core/tests/watch_webdav.rs` — stateful raw-TcpListener stub WebDAV server (pattern: `tests/llm_provider_thinking.rs`); axum dev-dep is impossible (CI guard greps whole core Cargo.toml)
- [ ] `migrations/0009_watch_state.sql` — per-file `(vault_id, source_id, path)` + etag/size/sha256/last_seen_at; `sync_state` gains `sync_unsupported` column
- [ ] `roxmltree = "0.21"` workspace dep (pure-Rust XML for 207-multistatus parsing)

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| Poll against a real LAN WebDAV server (Apache mod_dav / Nextcloud) | SRCC-01 | CI has no real WebDAV server; stub cannot prove real-server header quirks | Phase 4 UAT: point `[[watch]]` at a real server, add/change/remove a file, observe jobs + log.md |

---

## Validation Sign-Off

- [x] All tasks have `<automated>` verify or Wave 0 dependencies
- [x] Sampling continuity: no 3 consecutive tasks without automated verify
- [x] Wave 0 covers all MISSING references
- [x] No watch-mode flags
- [x] Feedback latency < 120s
- [x] `nyquist_compliant: true` set in frontmatter

**Approval:** approved 2026-11-01 (plan-checker VERIFICATION PASSED, 0 blockers)
