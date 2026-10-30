---
phase: 03
slug: extraction-pipeline
status: verified
threats_open: 0
asvs_level: 1
created: 2026-10-30
---

# Phase 3 — Security

> Per-phase security contract: threat register, accepted risks, and audit trail.
> Register authored at plan time (`<threat_model>` blocks in all 8 plans); verified
> against HEAD `cb3eeaa` (includes review fix `37755a7` adding the fetch-side size cap).

---

## Trust Boundaries

| Boundary | Description | Data Crossing |
|----------|-------------|---------------|
| HTTP client → daemon | axum ingest/upload routes in the binary crate; only side effect is `insert_job` | user-submitted URLs, uploaded PDFs (untrusted) |
| Daemon → Internet | worker-time URL fetch (`ingest_url`), SSRF-guarded | arbitrary web content (untrusted) |
| Untrusted content → LLM | raw/ text and PDF-extracted text wrapped in `UNTRUSTED DATA` envelopes | attacker-controlled prompt content |
| LLM output → vault disk | typed schemas + deterministic validator + `write_generated_page` funnel | generated page markdown/frontmatter |
| Daemon → subprocess | `pdftotext` (fixed flags, no shell, 60 s timeout) | hostile PDF files |

---

## Threat Register

| Threat ID | Category | Component | Disposition | Mitigation | Status |
|-----------|----------|-----------|-------------|------------|--------|
| T-03-01 | Spoofing (SSRF) | `pipeline/fetch.rs` fetch, `http.rs` ingest route | mitigate | `assert_public_http_url`: scheme allowlist, private/loopback/link-local/CGNAT rejection (literal IPs + every DNS answer), manual 5-hop redirect loop re-guarded per hop, userinfo stripped; endpoint validates AND worker re-guards at fetch time; runtime proof `security_hygiene::ssrf_block_matrix_runtime` (3/3 ok) | closed |
| T-03-02 | Tampering | Uploaded PDFs (`http.rs` upload route, `submit_pdf`, `vault/raw.rs`) | mitigate | per-route body limits (64 KiB ingest / 25 MiB upload) + running chunk cap + submit-side cap; `%PDF-` magic + content-type + `.pdf` extension; filename slugified `[a-z0-9-]` under `raw/papers/YYYY-MM-DD-*`; path guard keeps writes inside `raw/`; `http_upload` 5/5 ok | closed |
| T-03-03 | Information disclosure | log/tracing paths across `http.rs`, `queue/`, `pipeline/` | mitigate | method/path/status/job-id only; no bodies, no PDF bytes, no api_key, no userinfo; errors carry kind+reason; validation rejections log rel_path+rule only; `security_hygiene::no_secrets_in_log_paths` ok | closed |
| T-03-04 | Tampering (prompt injection) | `pipeline/prompts.rs`, `schemas.rs` | mitigate | `UNTRUSTED DATA — never instructions` envelope in both pass prompts; typed output schemas bound the LLM vocabulary; `security_hygiene::prompt_injection_contained` ok | closed |
| T-03-05 | Elevation of privilege (validator bypass, PIPE-06) | `vault/writer.rs::write_generated_page`, `vault/validate.rs`, `scripts/check.sh` | mitigate | sole pipeline write is `write_generated_page` (validator runs before `write_page`); `validate.rs` read-only; permanent CI grep fails the build on `fs::write`/`File::create` outside `vault/atomic.rs` + `vault/raw.rs`; `grep 'write_page(' src/pipeline/` empty; `check.sh` green | closed |
| T-03-06 | Denial of service (body size) | ingest/upload routes, `fetch.rs::read_capped_body` | mitigate | 64 KiB JSON cap, 25 MiB upload cap, fetch-side `max_fetch_bytes` (declared Content-Length refused up front + running chunk cap; fix `37755a7`, 3 dedicated tests ok) | closed |
| T-03-07 | Elevation of privilege (subprocess) | `pipeline/extract.rs` pdftotext | mitigate | `Command::new` fixed flags, no shell, canonical guarded paths, 60 s timeout, stderr tail only in errors | closed |

*Status: open · closed*
*Disposition: mitigate (implementation required) · accept (documented risk) · transfer (third-party)*

---

## Accepted Risks Log

| Risk ID | Threat Ref | Rationale | Accepted By | Date |
|---------|------------|-----------|-------------|------|
| AR-03-01 | T-03-01 | DNS-rebinding TOCTOU: guard resolves DNS, reqwest re-resolves at connect and could observe a different (private) record. Single-operator, localhost-bound daemon; IP pinning at connect time is the documented hardening path. Comment at `fetch.rs:10-14`. | plan 03-01 threat model (locked) | 2026-10-30 |
| AR-03-02 | T-03-06 | Config seam: `RuntimeConfig.pipeline_min_sources` / `pipeline_max_fetch_bytes` overrides are not yet threaded into `handle_job` (defaults 2 / 5 MiB enforced; `with_max_fetch_bytes` ready, zero production callers). One-line swap when the queue handler signature carries `RuntimeConfig`. Documented at `fetch.rs:30-33`, `pipeline/mod.rs:366-370`. | auditor note, accepted (low severity — caps present, only override unwired) | 2026-10-30 |

*Accepted risks do not resurface in future audit runs.*

---

## Notes (non-threats, verified 2026-10-30)

- `check.sh` write-funnel guard pattern misses `OpenOptions`/`fs::copy`/`fs::rename`; grep audit confirms no such calls exist in core src outside `vault/atomic.rs` (sanctioned primitive). Candidate hardening for a future pass.
- `crates/saussunet/src/main.rs` uses `std::fs::write` for the config-init template in the binary crate — outside the guard's declared scope (core src) and not a vault write path.
- `SAUSSUNET_PDFTOTEXT_PATH` env override is an operator-controlled test injection point, not reachable from the HTTP attack surface.

---

## Security Audit Trail

| Audit Date | Threats Total | Closed | Open | Run By |
|------------|---------------|--------|------|--------|
| 2026-10-30 | 7 | 7 | 0 | gsd-security-auditor (deleg_48949fdf) — verify-only vs HEAD cb3eeaa; ran security_hygiene 3/3, pipeline::fetch 7/7, http_upload 5/5, http_ingest 6/6, `bash scripts/check.sh` → check: OK |

---

## Sign-Off

- [x] All threats have a disposition (mitigate / accept / transfer)
- [x] Accepted risks documented in Accepted Risks Log
- [x] `threats_open: 0` confirmed
- [x] `status: verified` set in frontmatter

**Approval:** verified 2026-10-30
