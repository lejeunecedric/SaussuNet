---
phase: 4
slug: continuous-source-watching
status: verified
threats_open: 0
asvs_level: 1
created: 2026-11-03
---

# Phase 4 — Security

> Per-phase security contract: threat register, accepted risks, and audit trail.
> Register authored at plan time (7 `<threat_model>` blocks), consolidated into 8 threat classes + 1 documented decision. Verified by read-only auditor against HEAD `b3b13f6` (post review-fix commits `473e64f` H1, `5da585f` M2, `2963c5c` M1).

---

## Trust Boundaries

| Boundary | Description | Data Crossing |
|----------|-------------|---------------|
| Operator config → daemon | `[[watch]]` entries (URLs, basic-auth creds) in `config.toml` are operator-trusted (D-03) | WebDAV credentials (SecretString) |
| WebDAV server → daemon | PROPFIND/sync-report 207 bodies, file bytes, hrefs, sync tokens — untrusted even from a trusted-operator server | XML, binary content, path strings |
| Daemon → vault FS / queue | raw writes via `write_raw` funnel; jobs enqueued for the Phase 3 pipeline | file content into `raw/watch/`, job payloads |

---

## Threat Register

| Threat ID | Category | Component | Disposition | Mitigation | Status |
|-----------|----------|-----------|-------------|------------|--------|
| T-01 | Information disclosure (credential leakage) | config, watch/dav.rs, watch/mod.rs, loop WARN | mitigate | SecretString-only (`watch_password_never_in_debug`); userinfo URLs rejected at resolve; errors sanitized to scheme+host+path (`error_string_has_no_password`); `last_error` sanitized + newline-stripped; `reveal()` call sites confined by `no_secrets_in_log_paths` source audit | closed |
| T-02 | Denial of service (config/resource) | config resolve, spawn_watcher | mitigate | zero interval/depth rejected (`zero_interval_rejected`, `zero_depth_cap_rejected`); one task per config entry; 30 s reqwest timeout; missed ticks now skipped (`MissedTickBehavior::Skip`, fix post-audit-note — review L3) | closed |
| T-03 | Tampering / DoS (untrusted XML) | watch/multistatus.rs, watch/dav.rs | mitigate | roxmltree: no DTD fetch, errors on undefined entities; 8 MiB multistatus cap BEFORE parse (`MAX_MULTISTATUS_BYTES` via shared `read_capped`, declared + lying Content-Length both refused); streaming download cap `max_fetch_bytes` (`get_bytes_respects_size_cap`) | closed |
| T-04 | Elevation / traversal (crafted hrefs) | watch/dav.rs url_for, vault/raw.rs | mitigate | `url_for` rejects `.`/`..` segments post-decode at the single choke point covering propfind_children + sync_collection + get_bytes (fix `2963c5c`, tests `url_for_rejects_dot_segments`, `dot_segment_href_cannot_escape_watch_root`); parsed paths are opaque DB keys; fs writes funnel `write_raw::sanitize_component` | closed |
| T-05 | Integrity (partial-walk removal storm) | watch/mod.rs poll_once / apply_sync_report | mitigate | `walk_complete` gate — removals only after a fully completed walk (`poll_failure_records_last_error` asserts zero prunes); sync path: diff-based removals neutralized, only explicit 404 items prune | closed |
| T-06 | Integrity (log injection) | watch/mod.rs log lines | mitigate | `sanitize_line` strips CR/LF at every log/state line site (`log_injection_via_filename_is_neutralized`) | closed |
| T-07 | Tampering (sync token handling) | watch/dav.rs, store/sync_state.rs | mitigate | token opaque, 4096-char cap before persist, XML-escaped into body, parameter-bound sqlx, never URL/command-interpolated; stale retry bounded to one per poll; latch only on HTTP-level non-207 signals — transport errors do NOT latch (fix `5da585f`, tests `transport_error_first_poll_does_not_latch_sync_unsupported`, `http_400_first_poll_latches_sync_unsupported`) | closed |
| T-08 | Availability (silent watcher death) | spawn_watcher | accept | JoinHandle kept + aborted on ctrl_c; same posture as Phase 2 worker pool; Phase 5 UI will surface last-poll status (code comment `watch/mod.rs:630-633`) | closed |
| D-03 | SSRF non-applicability | config resolve | accept (documented decision) | Watch URLs operator-trusted by CONTEXT D-03; no public-IP guard by design, echoed in `resolve_watch` doc + runtime WARN for plain http; no non-operator influence — no HTTP route adds watch sources (`http.rs` has zero watch references) | closed |

*Status: open · closed*
*Disposition: mitigate (implementation required) · accept (documented risk) · transfer (third-party)*

---

## Accepted Risks Log

| Risk ID | Threat Ref | Rationale | Accepted By | Date |
|---------|------------|-----------|-------------|------|
| AR-04-01 | T-08 | Watcher task panic stops one source silently; JoinHandle kept + abort-on-shutdown, Phase 5 UI surfaces last-poll status — same v1 posture as worker pool | Plan 04-06 threat model (register-time accept) | 2026-11-01 |
| AR-04-02 | D-03 | Watch URLs may point at private addresses by design (LAN self-hosted WebDAV is the target deployment); SSRF guard deliberately not applied; trust root is the operator-authored config only | CONTEXT D-03 (user-facing default, plan-locked) | 2026-11-01 |
| AR-04-03 | T-03/T-07 test coverage | No dedicated tests for the 8 MiB multistatus constant and the 4096-char token cap specifically; both code paths verified by the auditor and the shared `read_capped` cap path is tested (`get_bytes_respects_size_cap`) | Auditor note, orchestrator accepted | 2026-11-03 |

*Accepted risks do not resurface in future audit runs.*

---

## Security Audit Trail

| Audit Date | Threats Total | Closed | Open | Run By |
|------------|---------------|--------|------|--------|
| 2026-11-03 | 9 (T-01..T-08 + D-03) | 9 | 0 | gsd-security-auditor (read-only, HEAD `b3b13f6`; verdict SECURED) |

Post-audit inline fix: `MissedTickBehavior::Skip` one-liner (auditor T-02 note / review L3), committed after audit.

---

## Sign-Off

- [x] All threats have a disposition (mitigate / accept / transfer)
- [x] Accepted risks documented in Accepted Risks Log
- [x] `threats_open: 0` confirmed
- [x] `status: verified` set in frontmatter

**Approval:** verified 2026-11-03
