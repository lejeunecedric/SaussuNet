---
phase: 4
slug: continuous-source-watching
status: draft
nyquist_compliant: false
wave_0_complete: false
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
| _pending plans_ | — | — | SRCC-01/02 | — | — | — | — | ❌ W0 | ⬜ pending |

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

- [ ] All tasks have `<automated>` verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references
- [ ] No watch-mode flags
- [ ] Feedback latency < 120s
- [ ] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
