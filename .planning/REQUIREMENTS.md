# Requirements: SaussuNet

**Defined:** 2026-10-25
**Core Value:** Feed raw sources (Drive, WebDAV, web, MCP) and get, continuously, an up-to-date, interlinked, structured llm-wiki vault — with no manual note writing.

## v1 Requirements

Requirements for initial release (lean v1). Each maps to roadmap phases.

### Vault Core

- [x] **VLET-01**: The system writes vault pages following llm-wiki conventions: YAML frontmatter (title/created/updated/type/tags/sources), `[[wikilinks]]` (min 2 outbound per page), `index.md` catalog, append-only `log.md`, immutable `raw/` layer
- [x] **VLET-02**: A single vault-writer component is the sole author of vault files; all writes are atomic (write-temp + rename) so a crash never leaves a half-written page
- [x] **VLET-03**: Human edits to generated pages are preserved across regeneration (three-way merge via per-section `gen_hash`)
- [x] **VLET-04**: The data model is multi-vault-ready (config, SQLite schema, paths, queues carry a vault id) even though v1 ships one vault

### Ingestion Pipeline

- [x] **PIPE-01**: User can submit a web URL or PDF; the system extracts text, stores it under `raw/` with source frontmatter (`source_url`, `ingested`, `sha256`), and produces/updates entity and concept pages
- [x] **PIPE-02**: The pipeline skips unchanged sources (sha256 match) and flags changed ones for re-ingestion (source drift)
- [x] **PIPE-03**: Pages are created only above thresholds (entity/concept appears in 2+ sources or is central to one); passing mentions never create pages
- [x] **PIPE-04**: Near-duplicate entities are prevented by an entity registry with aliases and resolve-before-create matching
- [x] **PIPE-05**: Generated pages carry provenance (sources frontmatter, `^[raw/...]` markers on multi-source claims) and optional `confidence`/`contested` flags; contradictions are flagged, never auto-resolved
- [x] **PIPE-06**: Every generated page passes a deterministic write-path validator (link whitelist from index, frontmatter schema, provenance present) before it reaches disk — the LLM never writes to the vault directly

### LLM Provider

- [x] **LLMP-01**: User can configure an LLM provider via an abstract `LlmProvider` interface: any OpenAI-compatible endpoint (covers Ollama/llama.cpp local on Strix Halo and cloud APIs)
- [x] **LLMP-02**: Extraction workers run in a bounded pool sized to LLM capacity; a deterministic mock provider makes the whole pipeline testable without a real LLM

### Source Watching

- [x] **SRCC-01**: User can configure a WebDAV folder as a watched source; the system polls it on an interval, detects new/changed/removed files (etag/size), and enqueues ingestion jobs (Depth:1 walk with depth cap, never Depth: infinity)
- [x] **SRCC-02**: Watcher state (etags, pageTokens, last-check) persists in SQLite and survives restarts; enqueue is sha256-gated so re-polls of unchanged files are no-ops
- [x] **SRCC-03**: Ingestion jobs are durable (SQLite queue with atomic claim, retry states, claimed-at expiry) — a crash mid-batch resumes without loss or duplication

### OKF Output (Phase 04.1 insertion)

- [ ] **OKF-01**: The vault writer emits OKF v0.2 frontmatter natively: descriptive `type` derived from the page directory, `title`, one-line `description`, ISO 8601 UTC `timestamp`, `generated: {by: process:saussunet/<version>, at}` — `verified`/`status`/`stale_after` are never emitted without evidence
- [ ] **OKF-02**: Generated pages use bundle-relative markdown links (`[text](/path.md)`) instead of wikilinks; the write-path validator's link whitelist enforces the same
- [ ] **OKF-03**: Provenance `^[raw/...]` markers become markdown footnotes keyed to `sources[].id` (`[^id]` body markers, definitions at the end — amended 2026-11-18 per Phase 04.1 D-12, superseding the v0.1-form numbered citations + `# Citations` section); `sources:` frontmatter becomes `[{resource, id, last_modified}]` entries
- [ ] **OKF-04**: Index and log follow OKF §6/§7: index files carry no frontmatter (root declares `okf_version: "0.2"`), `log.md` entries group under `## YYYY-MM-DD` headings, newest first
- [ ] **OKF-05**: Every vault written by SaussuNet passes OKF §9 conformance — every non-reserved .md has parseable frontmatter with a non-empty `type` — checked by the write-path validator; broken cross-links are tolerated
- [ ] **OKF-06**: `saussunet migrate-okf` converts an existing llm-wiki vault in place to a conformant OKF bundle (Rust port of the B3Wiki converter, validated against B3Wiki fixtures) and refuses an already-converted vault
- [ ] **OKF-07**: The vault's SCHEMA.md template describes the OKF conventions (types, links, citations, trust fields) so consuming agents read the vault's own contract

### Health Lint

- [ ] **LINT-01**: The system runs a health lint over the vault (orphan pages, broken wikilinks, index completeness, frontmatter validity, tag taxonomy, raw sha256 drift, contested pages, oversized pages, log rotation) after each batch and on demand
- [ ] **LINT-02**: Lint reports are persisted and surfaced in the UI with severity grouping and suggested actions

### Review & Monitoring UI

- [ ] **REVI-01**: User can see a review queue of pending page proposals with before/after diffs and approve or reject each (destructive operations — create/merge/archive — always go through review; minor updates may auto-publish per-type policy)
- [ ] **REVI-02**: User can watch pipeline state in the UI: queued/running/failed jobs, per-source status, and errors with actionable messages
- [ ] **REVI-03**: User can configure sources and provider settings from the web UI (no hand-edited config required for the v1 happy path)

### MCP Exposure

- [ ] **MCPX-01**: The vault is exposed as a read-only MCP server (Streamable HTTP on `/mcp`): tools to list index, read a page, search pages (FTS), and fetch the latest lint report

### Preview

- [ ] **PREV-01**: User can preview the vault as an mdbook site from the UI; builds are triggered by approved batches (debounced) and fail LOUD if the mdbook CLI is missing from PATH

### Platform & Deployment

- [x] **INFR-01**: The project is a Cargo workspace: `crates/saussunet-core` (library, zero axum/HTTP-server dependency) + `crates/saussunet` (standalone web binary owning all transports)
- [x] **INFR-02**: The binary runs self-hosted with SQLite (WAL) and no external services; localhost binds by default
- [ ] **INFR-03**: Deployment to our server works via a deploy script whose server config lives in a gitignored file (example template committed, real config never)

## v2 Requirements

Deferred (v1.x then v2+). Tracked but not in current roadmap.

### Sources & Freshness (v1.x)

- **SRCX-01**: Google Drive watched source (OAuth2 + delta API; consent published to production to avoid the 7-day Testing refresh-token expiry)
- **SRCX-02**: Inbound MCP pointer sources: vault stores pointer notes only (`type: pointer`, `mcp_uri`, `last_verified_at` + ok/stale/dead status), content streamed on demand, never copied
- **SRCX-03**: Freshness repair (`refresh --stale`): re-ingest stale sources surfaced by lint
- **SRCX-04**: Multi-vault actually enabled (second real vault)

### Ontology (v1.x → v2)

- **ONTO-01**: Optional YAML ontology: classes → page types, typed relations → typed wikilinks; specializes SCHEMA.md
- **ONTO-02**: OWL/RDF import/export behind a cargo `rdf` feature (sophia or oxigraph — decided by spike)

### Quality & Ops (v1.x → v2)

- **QUAL-01**: Git auto-commit per pipeline batch (rollback/history of regenerations)
- **QUAL-02**: Numeric eval: health score, citation coverage, per-page distribution
- **QUAL-03**: Hybrid embeddings + BM25 search once FTS+index saturates (~500+ pages)
- **QUAL-04**: Assisted contradiction resolution (auto proposal + human validation)
- **QUAL-05**: MoriNet library integration (`saussunet-core` as a MoriNet dependency; requires sqlx major-version alignment)

## Out of Scope

| Feature | Reason |
|---------|--------|
| Proprietary vault format | Stay on standard Obsidian/llm-wiki; portability is the point |
| Real-time collaborative editing | WebSocket/CRDT complexity unjustified for personal self-hosted use |
| Multi-user accounts/roles | Self-hosted single operator in v1/v2 |
| Replacing MoriNet | SaussuNet builds, MoriNet serves; integration is a lib dependency, not a merge |
| Encryption at rest | Rely on the server filesystem |
| Automatic contradiction resolution | Anti-feature: LLM auto-resolution hardens hallucinations; flag + human review instead |
| Vector/embedding search in v1 | Anti-feature at personal-corpus scale: FTS + index-first is cheaper and auditable |
| Freeform tags outside taxonomy | Anti-feature: tag sprawl decays the vault; taxonomy lint enforces SCHEMA.md |
| Publishing without review for destructive ops | Anti-feature: create/merge/archive always gated by the review queue |

## Traceability

Which phases cover which requirements. Updated during roadmap creation.

| Requirement | Phase | Status |
|-------------|-------|--------|
| VLET-01 | Phase 1 | Complete |
| VLET-02 | Phase 1 | Complete |
| VLET-03 | Phase 1 | Complete |
| VLET-04 | Phase 1 | Complete |
| INFR-01 | Phase 1 | Complete |
| INFR-02 | Phase 1 | Complete |
| LLMP-01 | Phase 2 | Complete |
| LLMP-02 | Phase 2 | Complete |
| SRCC-03 | Phase 2 | Complete |
| PIPE-01 | Phase 3 | Complete |
| PIPE-02 | Phase 3 | Complete |
| PIPE-03 | Phase 3 | Complete |
| PIPE-04 | Phase 3 | Complete |
| PIPE-05 | Phase 3 | Complete |
| PIPE-06 | Phase 3 | Complete |
| SRCC-01 | Phase 4 | Complete |
| SRCC-02 | Phase 4 | Complete |
| LINT-01 | Phase 5 | Pending |
| LINT-02 | Phase 5 | Pending |
| REVI-01 | Phase 5 | Pending |
| REVI-02 | Phase 5 | Pending |
| REVI-03 | Phase 5 | Pending |
| MCPX-01 | Phase 6 | Pending |
| PREV-01 | Phase 6 | Pending |
| INFR-03 | Phase 6 | Pending |
| OKF-01 | Phase 04.1 | Pending |
| OKF-02 | Phase 04.1 | Pending |
| OKF-03 | Phase 04.1 | Pending |
| OKF-04 | Phase 04.1 | Pending |
| OKF-05 | Phase 04.1 | Pending |
| OKF-06 | Phase 04.1 | Pending |
| OKF-07 | Phase 04.1 | Pending |

**Coverage:**
- v1 requirements: 32 total (25 original + 7 OKF added for Phase 04.1 insertion)
- Mapped to phases: 32
- Unmapped: 0 ✅

---
*Requirements defined: 2026-10-25*
*Last updated: 2026-10-25 after initial definition (lean v1 scope chosen by user)*
