# Roadmap: SaussuNet

## Overview

SaussuNet is built from the ground up around the two things that cannot be retrofitted: the single-writer vault layer (atomic writes, `gen_hash` edit protection, multi-vault-ready schema) and the durable job queue with a deterministic write-path validator. Once the vault core exists, the LLM abstraction (mock first, any OpenAI-compatible endpoint second) and the extraction pipeline turn raw documents into validated page proposals. Continuous watching then makes the vault compound on its own, the lint + review UI makes the automation trustworthy, and the final phase exposes the vault outward (read-only MCP, mdbook preview) and puts it on our server. Each phase delivers an end-to-end, observable capability — Vertical MVP mode.

## Phases

**Phase Numbering:**
- Integer phases (1, 2, 3): Planned milestone work
- Decimal phases (2.1, 2.2): Urgent insertions (marked with INSERTED)

Decimal phases appear between their surrounding integers in numeric order.

- [x] **Phase 1: Workspace Foundations + Vault Writer** - Cargo workspace, SQLite store, and the single-writer llm-wiki vault layer (atomic writes, gen_hash merge, multi-vault-ready schema) (completed 2026-10-25)
- [x] **Phase 2: LLM Provider + Durable Job Queue** - Abstract LlmProvider (mock + OpenAI-compatible), bounded worker pool, crash-safe SQLite job queue (completed 2026-10-26)
- [ ] **Phase 3: Extraction Pipeline** - Submit a web URL or PDF and get validated, deduplicated, provenance-carrying llm-wiki pages
- [ ] **Phase 4: Continuous Source Watching** - WebDAV watched sources polled on an interval; the vault updates itself and survives restarts
- [ ] **Phase 5: Health Lint + Review & Monitoring UI** - Lint reports, review queue with diffs, pipeline monitoring, all config from the web UI
- [ ] **Phase 6: Exposure + Deployment** - Read-only MCP endpoint, mdbook preview from the UI, deploy to our server with gitignored config

## Phase Details

### Phase 1: Workspace Foundations + Vault Writer
**Goal:** A self-hosted binary owns a standards-compliant llm-wiki vault through a single atomic writer, with human edits protected and the schema ready for many vaults.
**Mode:** mvp
**Depends on:** Nothing (first phase)
**Requirements:** INFR-01, INFR-02, VLET-01, VLET-02, VLET-03, VLET-04
**Success Criteria** (what must be TRUE):
  1. Running the binary creates a vault with SCHEMA.md, index.md, log.md, an immutable raw/ layer, and pages with full YAML frontmatter and ≥2 outbound wikilinks — openable in Obsidian unchanged
  2. The vault-writer component is the only author of vault files and every write is atomic (temp + rename): killing the process mid-write never leaves a half-written page
  3. A human edit made to a generated page survives the next regeneration (three-way merge via per-section gen_hash recorded from the first write)
  4. Config, SQLite schema, paths, and queues all carry a vault id, even though v1 ships exactly one vault
  5. The binary runs with SQLite in WAL mode, no external services, and binds to localhost by default
**Plans**: TBD

### Phase 2: LLM Provider + Durable Job Queue
**Goal:** Jobs flow through a crash-safe queue to extraction workers backed by a swappable LLM provider — fully testable with a deterministic mock before any real model is configured.
**Mode:** mvp
**Depends on:** Phase 1
**Requirements:** LLMP-01, LLMP-02, SRCC-03
**Success Criteria** (what must be TRUE):
  1. User configures any OpenAI-compatible endpoint (Ollama/llama.cpp on the Strix Halo box or a cloud API) via the abstract LlmProvider interface and a job completes against it
  2. A deterministic mock provider drives the entire pipeline in tests — CI passes with no real LLM anywhere
  3. Jobs live in a SQLite queue with atomic claim, retry states, and claimed-at expiry: killing the process mid-batch resumes with no loss and no duplication
  4. Extraction workers run in a bounded pool sized to configured LLM capacity (never an unbounded fan-out)
**Plans**: TBD

### Phase 3: Extraction Pipeline
**Goal:** Submitting a web URL or PDF produces entity and concept pages in the vault — threshold-gated, deduplicated, provenance-carrying, and validated before they touch disk.
**Mode:** mvp
**Depends on:** Phase 2
**Requirements:** PIPE-01, PIPE-02, PIPE-03, PIPE-04, PIPE-05, PIPE-06
**Success Criteria** (what must be TRUE):
  1. User submits a web URL or PDF; the extracted text lands under raw/ with source_url/ingested/sha256 frontmatter and entity/concept pages are created or updated
  2. Re-submitting an unchanged source is a no-op (sha256 match); a changed source is flagged for re-ingestion as source drift
  3. Pages are created only above thresholds (entity/concept in 2+ sources or central to one); passing mentions never create pages, and near-duplicates are prevented by the entity registry (aliases, resolve-before-create)
  4. Generated pages carry sources frontmatter and `^[raw/...]` provenance markers, with contradictions flagged (contested) rather than auto-resolved
  5. Every page reaching disk passed the deterministic write-path validator (link whitelist from index, frontmatter schema, provenance present) — the LLM never writes to the vault directly
**Plans**: TBD

### Phase 4: Continuous Source Watching
**Goal:** The vault compounds on its own: a configured WebDAV folder is polled continuously, and watcher state survives restarts without re-ingesting unchanged files.
**Mode:** mvp
**Depends on:** Phase 3
**Requirements:** SRCC-01, SRCC-02
**Success Criteria** (what must be TRUE):
  1. User configures a WebDAV folder as a watched source; new, changed, and removed files are detected on the poll interval (etag/size, Depth:1 walk with a depth cap) and ingestion jobs are enqueued automatically
  2. Watcher state (etags, pageTokens, last-check) persists in SQLite: restarting the server resumes watching with no full re-scan
  3. Re-polling a folder of unchanged files enqueues nothing (sha256-gated enqueue)
**Plans**: TBD

### Phase 5: Health Lint + Review & Monitoring UI
**Goal:** The human is the guardrail: lint runs after every batch, destructive changes wait in a review queue with diffs, and everything is configured and monitored from the web UI.
**Mode:** mvp
**Depends on:** Phase 4
**Requirements:** LINT-01, LINT-02, REVI-01, REVI-02, REVI-03
**Success Criteria** (what must be TRUE):
  1. A health lint (orphans, broken wikilinks, index completeness, frontmatter validity, tag taxonomy, raw sha256 drift, contested pages, oversized pages, log rotation) runs after each batch and on demand
  2. Lint reports are persisted and shown in the UI grouped by severity with suggested actions
  3. User sees a review queue of pending page proposals with before/after diffs and approves or rejects each; create/merge/archive always go through review, minor updates auto-publish per-type policy
  4. User watches pipeline state in the UI: queued/running/failed jobs, per-source status, errors with actionable messages
  5. Sources and provider settings are configured from the web UI — the v1 happy path needs no hand-edited config files
**Plans**: TBD

### Phase 6: Exposure + Deployment
**Goal:** The finished vault reaches the outside world — read-only over MCP, previewable as an mdbook site — and the whole server runs on our infrastructure.
**Mode:** mvp
**Depends on:** Phase 5
**Requirements:** MCPX-01, PREV-01, INFR-03
**Success Criteria** (what must be TRUE):
  1. An external MCP client on the Streamable HTTP `/mcp` endpoint can list the index, read a page, search pages (FTS), and fetch the latest lint report — read-only
  2. User previews the vault as an mdbook site from the UI; builds are triggered by approved batches (debounced) and a missing mdbook CLI fails LOUD with an actionable error
  3. `deploy.sh` deploys to our server (MoriNet pattern: systemd/nginx) reading hosts/paths/credentials from a gitignored file — the repo contains only the example template
**Plans**: TBD

## Progress

**Execution Order:**
Phases execute in numeric order: 1 → 2 → 3 → 4 → 5 → 6

| Phase | Plans Complete | Status | Completed |
|-------|----------------|--------|-----------|
| 1. Workspace Foundations + Vault Writer | 4/4 | Complete    | 2026-10-25 |
| 2. LLM Provider + Durable Job Queue | 5/5 | Complete   | 2026-10-26 |
| 3. Extraction Pipeline | 2/8 | In Progress|  |
| 4. Continuous Source Watching | 0/? | Not started | - |
| 5. Health Lint + Review & Monitoring UI | 0/? | Not started | - |
| 6. Exposure + Deployment | 0/? | Not started | - |
