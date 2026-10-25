# Project Research Summary

**Project:** SaussuNet — Rust server that continuously builds/maintains llm-wiki knowledge vaults from watched sources (Drive, WebDAV, web/PDF, MCP), with web UI review queue, optional ontology, bidirectional MCP, mdbook preview.
**Domain:** Self-hosted Rust ETL/LLM pipeline server + Obsidian-compatible knowledge-vault compiler (sibling of MoriNet)
**Researched:** 2026-10-25
**Confidence:** HIGH on stack and architecture (versions verified on crates.io, MoriNet sibling patterns); MEDIUM-HIGH on features (one deep competitor analysis, rest from snippets); HIGH on pitfalls (externally corroborated + MoriNet field lessons)

## Executive Summary

SaussuNet is a **continuous ETL pipeline server**, not a chat agent: source watchers emit events into a durable SQLite job queue, a bounded LLM worker pool produces validated page proposals, and a single vault-writer task is the sole author of the Markdown vault (FS = source of truth; SQLite = rebuildable projection). This is the MoriNet philosophy (single binary, axum + sqlx/SQLite, mdbook subprocess, rmcp) extended to the "fabrication" side.

The recommended stack is MoriNet-aligned: tokio 1.54, axum 0.8.9, sqlx 0.9 (SQLite, WAL), reqwest 0.13 (rustls), rmcp 3.7, askama 0.16 + vendored htmx SSR, rust-embed, serde_yml, pulldown-cmark, git2 0.21, notify 8.2 — with a custom `LlmProvider` trait over an OpenAI-compatible HTTP client (no LLM framework). The competitive space is young: llm-wiki-compiler (llmwiki) industrialized compile/review/lint/MCP but is a one-shot human-triggered CLI. SaussuNet's differentiators — continuous watching, inbound MCP pointer sources, standard YAML/OWL/RDF ontology, real review UI, embeddable `saussunet-core` — are exactly the gaps it leaves.

The main risks are domain-intrinsic, not tech-intrinsic: source drift (silent re-ingestion or silent staleness), near-duplicate entity pages, and hallucinated/unsourced content becoming durable "knowledge" — plus the Google Drive OAuth 7-day Testing-status time bomb and WebDAV server non-uniformity. All have concrete, cheap-to-install preventions (sha256-gated enqueue, entity registry + resolve-before-create, deterministic write-path validator, single-writer + `gen_hash` edit protection) that must land in the **first phases**, not as later optimizations. Retrofitting single-writer or `gen_hash` after parallel jobs exist is a rewrite.

## Key Findings

### Recommended Stack

MoriNet-consistent Rust monolith: axum/tokio/sqlx/rmcp/git2 shared with the sibling project so `saussunet-core` can later be a MoriNet dependency without version conflicts. Full detail in [STACK.md](STACK.md).

**Core technologies:**
- tokio 1.54 + axum 0.8.9 + tower-http 0.7 — HTTP API/UI + MCP endpoint; rmcp's StreamableHttpService mounts directly on the router
- sqlx 0.9 (SQLite, WAL) — job queue, sync state, entity registry, lint reports; ⚠️ MoriNet is on sqlx 0.8 — align before lib integration
- rmcp 3.7 — outbound vault MCP server + inbound MCP client (pointer notes); reuses MoriNet Phase 10 experience
- Custom `LlmProvider` trait + reqwest 0.13 (rustls) over OpenAI-compatible API — covers Ollama/llama.cpp (Strix Halo ROCm) and clouds; genai/rig-core rejected as churn/overhead
- askama 0.16 + vendored htmx 2.x + rust-embed 8.13 — SSR UI, zero node toolchain, single binary
- serde_yml 0.0.13 (serde_yaml is deprecated), pulldown-cmark 0.13, gray_matter 0.3 — frontmatter/wikilinks/lint
- yup-oauth2 12.1 + manual Drive v3 REST (google-apis-rs is being archived), reqwest_dav 0.3, pdf_oxide 0.3 + lopdf 0.46, scraper + html2md — connectors
- git2 0.21, notify 8.2, mdbook 0.5.4 CLI subprocess, clap 4.6, thiserror/anyhow, backon 1.6 — supporting cast
- oxigraph/oxrdf/oxttl behind a `rdf` cargo feature — optional RDF/OWL, never on the default build path

### Expected Features

Full analysis in [FEATURES.md](FEATURES.md). Market is young; llmwiki is the reference competitor (one-shot CLI, read-only viewer, proprietary profile format).

**Must have (table stakes):**
- Ingestion pipeline → llm-wiki conventions (frontmatter, wikilinks, index.md/log.md, immutable raw/) — users expect this
- Dedup + sha256 source drift + page-creation thresholds (2+ sources) — the guardrails of the continuous regime
- Health lint (orphans, broken links, taxonomy, drift) + provenance markers + contradiction flags (`contested`)
- Human review queue with before/after diffs — the promised quality guardrail
- Incremental compilation (SQLite source→pages index) — economic condition for local LLM
- Obsidian compatibility, outbound MCP exposure, mdbook preview, abstract LLM provider

**Should have (differentiators):**
- Continuous source monitoring (poll + MCP streams) — no competitor does it; THE core value
- Inbound MCP pointer sources (never copy content) — unique
- Optional YAML→OWL/RDF ontology (standard formats vs llmwiki's proprietary CLP)
- Web review/edit UI + pipeline tracking; embeddable `saussunet-core` lib; multi-vault-ready schema

**Defer (v2+):**
- OWL/RDF import (YAML covers personal use), hybrid embeddings search (FTS + index first), assisted contradiction resolution, MoriNet lib integration (consume MCP first), compounding queries

**Anti-features to refuse:** automatic contradiction resolution without review, unrestricted auto-publish (creation/merge/archive always gated), freeform tags, copying MCP content into raw/, vector search as retrieval core.

### Architecture Approach

Three-layer shape (exposure / orchestration+domain / persistence) as a two-member Cargo workspace; full detail with diagrams in [ARCHITECTURE.md](ARCHITECTURE.md).

**Workspace layout:** `crates/saussunet-core` (lib — **zero axum, zero transport**; store/, scheduler, connectors/ behind `SourceConnector` trait, vault/ single-writer module, extract/, llm/ with mock, ontology/, mcp_server.rs logic-only) + `crates/saussunet` (binary — routes/, ui/, preview.rs, mcp_mount.rs); `migrations/` at workspace root shared by lib and bin.

**Major components:**
1. Source connectors + watch scheduler — poll (Drive changes.list/pageToken, WebDAV PROPFIND Depth:1 + ETag, web/PDF, MCP streams) → `SourceEvent`; token persisted only after successful enqueue
2. Durable SQLite job queue + bounded worker pool — atomic `UPDATE…RETURNING` claim, retry/dead-letter, LLM semaphore sized to real capacity (1–2 local, 4–8 cloud)
3. Vault writer (sole author) — batch-merged `PageProposal`s, atomic temp+rename writes, index/log updates once per batch
4. Lint + review + UI + mdbook preview + outbound MCP — all read the vault, write only through the writer/review path

**Core patterns:** durable queue (not in-memory); boundary traits (`SourceConnector`, `LlmProvider`, `Ontology`); vault FS = truth, SQLite rebuildable via `reindex`; single vault writer; deterministic validator between LLM output and disk.

**Build order (from ARCHITECTURE.md):** 1 workspace+store → 2 vault writer (no LLM) → 3 LLM trait+mock → 4 extraction pipeline → 5 connectors (web/PDF → WebDAV → Drive → MCP) → 6 scheduler → 7 ontology (YAML first) → 8 lint → 9 web UI → 10 mdbook preview → 11 outbound MCP → 12 MoriNet reusability hardening. v1 critical path: 1→2→3→4→5(web)→6→9(minimal).

### Critical Pitfalls

Top items from [PITFALLS.md](PITFALLS.md) (12 total, all mapped to phases):

1. **Source drift (silent re-ingestion or silent staleness)** — sha256-gated enqueue: cheap poll first, hash content, only enqueue on hash change; store `source_sha256` in note frontmatter. Part of the watcher contract, not an optimization.
2. **Near-duplicate entity pages** — entity registry (canonical + aliases) with resolve-before-create; fuzzy 0.7–0.9 band routes to review; periodic batched dedup pass, merges never silent.
3. **Hallucinated links / unsourced content** — link whitelist from index+registry, deterministic post-generation validator (links, frontmatter schema, provenance) between LLM and disk; never let the LLM write directly.
4. **Human edits silently overwritten** — store `gen_hash` from the first write (impossible retroactively); three-way merge → review queue on divergence; single-writer + atomic writes from day one (P1).
5. **Drive OAuth 7-day time bomb + WebDAV divergence** — publish consent screen to production (documented user setup step), persist refresh token, `invalid_grant` → loud actionable UI state; WebDAV: never `Depth: infinity`, `Depth: 1` walk, test against ≥2 real servers in CI.

Also notable: feedback loop (only `raw/` is evidence; watchers refuse vault-internal paths), MCP pointer rot (timeouts + circuit breaker + `status: ok|stale|dead`), secrets/prompt injection (creds never inside vault dir, localhost binds by default, secret scan pre-write).

### Cross-Document Conflicts (resolve during planning)

| Topic | STACK.md says | ARCHITECTURE.md says | Resolution |
|---|---|---|---|
| **sqlx version** | 0.9.0, with explicit warning MoriNet is on 0.8 | Store row says "sqlx 0.8" | Decide at phase 1: pin 0.9 now and migrate MoriNet later, or pin 0.8 if lib integration is imminent — two sqlx majors cannot coexist in one binary |
| **Google Drive** | Manual Drive v3 REST + yup-oauth2; google-apis-rs **archived — do NOT use** | Component table names `google-drive3` crate | STACK.md is right (archiving announced Sep 2025); use reqwest + yup-oauth2 |
| **PDF extraction** | pdf_oxide primary; pdf-extract explicitly rejected (~91% pass rate) | Mentions "pdf-extract / lol_pdf" | STACK.md wins: pdf_oxide + lopdf; spike to confirm |
| **WebDAV client** | reqwest_dav 0.3; `dav-client` does not exist on crates.io | Mentions "dav-client" as option | STACK.md wins: reqwest_dav or manual PROPFIND |
| **RDF/OWL libs** | oxigraph/oxrdf/oxttl (behind `rdf` feature) | sophia 0.10 / horned-owl | Unresolved — both viable; defer behind feature flag, decide in ontology spike (P7/step 7) |

## Implications for Roadmap

Suggested phase structure (aligns with PITFALLS.md provisional P1–P7 and ARCHITECTURE build order):

### Phase 1: Workspace foundations + vault core
**Rationale:** Single-writer vault layer, `gen_hash`, layer rules (raw/ = evidence) and the SQLite schema (multi-vault-ready) cannot be retrofitted — Pitfalls 6, 7, 4 all pin prevention here.
**Delivers:** Cargo workspace, config, migrations (sources/sync_state/jobs/entity_registry/page_registry), vault writer (SCHEMA/index/log, atomic writes, writer lock), llm-wiki format tests against a temp vault.
**Addresses:** llm-wiki conventions, Obsidian compatibility.
**Avoids:** Concurrent-write corruption, unrecoverable human-edit loss, feedback loop.

### Phase 2: LLM abstraction + extraction pipeline
**Rationale:** The pipeline is the core value; the deterministic validator and entity registry must be born with it.
**Delivers:** `LlmProvider` trait + OpenAI-compatible impl + mock, prompts, `PageProposal` validation (link whitelist, provenance, thresholds), worker loop, dedup/resolve-before-create, incremental contract.
**Uses:** reqwest/rustls, serde_yml, schemars-style JSON validation, backon.
**Implements:** Extraction workers + bounded pool + queue claim pattern.
**Avoids:** Hallucinated links, near-duplicates, cost blowup.

### Phase 3: Connectors + watch scheduler (continuous regime)
**Rationale:** Continuous watching is THE differentiator, but each connector needs the sha256-gated enqueue contract from phases 1–2.
**Delivers:** web/PDF first (end-to-end demo), then WebDAV, then Drive (OAuth production docs, `invalid_grant` state), then inbound MCP pointer notes; per-source poll loops with backoff + CancellationToken.
**Avoids:** Source drift, Drive time bomb, WebDAV divergence, MCP pointer rot.

### Phase 4: Lint + review queue + web UI
**Rationale:** Review is the guardrail that makes automation acceptable; it needs pipeline state and diffs to exist first.
**Delivers:** health lint + `lint_reports`, review UI (diff, approve/reject, `reviewed` flag), pipeline monitoring view, source/ontology config, manual triggers.
**Avoids:** Silent failures, review-queue flood, rubber-stamping.

### Phase 5: Exposure — outbound MCP + mdbook preview
**Rationale:** Both are read-only verticals over the vault, independent of the pipeline; outbound MCP is the MoriNet integration prerequisite.
**Delivers:** rmcp read-only tools (search/read/log) mounted on axum; debounced mdbook subprocess with LOUD failure; localhost-bind defaults.

### Phase 6: Ontology (YAML first) + hardening + deploy
**Rationale:** Deferrable without breaking the pipeline; RDF/OWL behind a feature flag pending spike.
**Delivers:** embedded default YAML ontology + validator wiring + ontology-violation lint; MoriNet reusability audit (zero axum in core); deploy.sh + gitignored deploy.local.sh (MoriNet pattern).

### Phase Ordering Rationale
- Vault writer before any LLM work: the vault format is testable without a model, and single-writer/gen_hash are unfixable retroactively.
- Queue + validator before connectors: every connector reuses the same enqueue contract; a connector can ship with jobs pending.
- UI after pipeline state exists: review without diffs/state is theater.
- Multi-vault decision (schema shape) forced in Phase 1 even though one vault ships (FEATURES.md dependency: conflicts with monolithic state).

### Research Flags
Phases likely needing deeper research during planning:
- **Phase 3 (Drive + WebDAV):** OAuth consent-screen production setup specifics, per-server WebDAV quirks — needs a real-server test harness design.
- **Phase 6 (ontology RDF/OWL):** sophia vs oxigraph unresolved; no OWL reasoner in Rust — spike required.
- **Phase 2 (local LLM structured output):** JSON-mode reliability on llama.cpp/Ollama ROCm gfx1151 — spike with the repair-prompt fallback.

Phases with standard patterns (skip research-phase):
- **Phase 1:** llm-wiki conventions fully documented (local skill + gist); MoriNet sqlx/migrations pattern.
- **Phase 5:** rmcp already proven in MoriNet Phase 10; mdbook subprocess pattern copied from MoriNet `sources.rs`.

## Confidence Assessment

| Area | Confidence | Notes |
|------|------------|-------|
| Stack | HIGH | Versions verified on crates.io 2026-10-25; a few Medium picks (pdf_oxide benchmark, reqwest_dav, askama+htmx) to confirm in use |
| Features | MEDIUM-HIGH | Deep analysis of llmwiki; others from snippets; core pattern from local llm-wiki skill |
| Architecture | HIGH | Structure/patterns solid; specific crate choices (PDF, RDF, WebDAV) flagged for spikes |
| Pitfalls | HIGH | Externally corroborated (Google OAuth docs, WebDAV RFC issues, MCP spec) + MoriNet field lessons |

**Overall confidence:** HIGH

### Gaps to Address
- sqlx 0.8-vs-0.9 alignment with MoriNet — decide in Phase 1 planning; add cargo-deny duplicate-version watch.
- pdf_oxide pass-rate claim is vendor-sourced — validate on a real PDF corpus during Phase 3.
- RDF library choice (oxigraph vs sophia) — defer behind `rdf` feature, spike in Phase 6.
- Multi-vault schema shape — force the decision in Phase 1 even if single-vault ships.
- llmwiki competitor evolves fast (v1.5.0, Oct 2026) — re-check feature matrix before v1.x planning.

## Sources

### Primary (HIGH confidence)
- crates.io API (programmatic check 2026-10-25) — ~70 crate versions
- Karpathy llm-wiki gist + local llm-wiki skill — 3-layer pattern, thresholds, lint rules, log rotation
- MoriNet repo + `.planning/` — reference stack, mdbook-subprocess pattern, field pitfalls (#13, #19, #24)
- Google OAuth docs + Drive API docs; WebDAV RFC issues list + litmus; MCP spec 2025-11-25; rmcp README (spec 2026-07-28)
- llm-wiki-compiler (atomicstrata) README v1.5.0 — full competitor teardown

### Secondary (MEDIUM confidence)
- obsidian-second-brain, Basic Memory, AuditToolbox Second Brain — feature landscape snippets
- glukhov.org, affine.pro, llmwikis.org — failure modes, review-gating practice
- pdf.oxide.fyi benchmark (vendor-claimed, not reproduced)

### Tertiary (LOW confidence)
- google-cloud-rust Drive coverage (archiving follow-up) — needs verification
- sophia/horned-owl maturity for the ontology import path — spike required

---
*Research completed: 2026-10-25*
*Ready for roadmap: yes*
