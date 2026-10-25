# SaussuNet

## What This Is

A Rust server (same philosophy as MoriNet) that **builds and maintains knowledge vaults** in llm-wiki format (Karpathy pattern: `SCHEMA.md`, `raw/`, `entities/`, `concepts/`, `index.md`, `log.md`), standard Obsidian-compatible. It watches sources (Google Drive, WebDAV, web, MCP), runs them through an LLM pipeline with an optional ontology, and produces a living interlinked vault — with a web UI for configuration, pipeline monitoring, review/editing and mdbook preview.

Two ways to live: **standalone (webapp)** or **library embeddable in MoriNet** (Rust workspace, core crate `saussunet-core`).

## Core Value

Feed raw sources (Drive, WebDAV, web, MCP) and get, **continuously**, an up-to-date, interlinked, structured llm-wiki vault (optional ontology) — with no manual note writing.

## Requirements

### Validated

<!-- Shipped and confirmed valuable. -->

(None yet — ship to validate)

### Active

<!-- Starting hypotheses, validated by shipping. -->

- [ ] Ingestion pipeline: sources → extraction/linking → llm-wiki notes (SCHEMA, index, log, wikilinks, frontmatter)
- [ ] File sources: Google Drive, WebDAV, documents (PDF/web) — poll/continuous watch
- [ ] Inbound MCP sources: the vault stores only **pointer notes** (access-point address), content read live via stream on demand
- [ ] Outbound MCP exposure: the vault served over MCP for external agent reading
- [ ] Optional ontology (YAML, OWL, RDF, schema): object classes, their display, and relations — otherwise default llm-wiki ontology
- [ ] Abstract LLM provider: local (Strix Halo / ROCm) or cloud API, interchangeable
- [ ] Web UI: source + ontology config, pipeline monitoring (ingestion, errors), review/editing of generated notes
- [ ] mdbook preview of the vault, configured like in MoriNet
- [ ] Rust workspace: `saussunet-core` (lib) + standalone web binary; MoriNet can depend on the lib later
- [ ] Deployment to our server (MoriNet pattern: deploy script + systemd/nginx), with server config kept in a **gitignored** file — never in the repo

### Out of Scope

- Proprietary vault format — stay on standard Obsidian/llm-wiki; MoriNet is not a format constraint
- Real-time collaborative editing — out of v1 (same reasoning as MoriNet)
- Multi-user accounts/roles — self-hosted personal use first
- Replacing MoriNet — SaussuNet builds, MoriNet serves; the core lib may be embedded, not merged
- Encryption at rest — rely on the server's filesystem

## Context

- Sibling of MoriNet (/home/c/Projects/Dev/MoriNet): same reference stack (Rust, axum, SQLite), same GSD workflow, same mdbook preview pattern. MoriNet **serves** vaults; SaussuNet **builds** them.
- Karpathy llm-wiki pattern: 3 layers (immutable raw/, agent-owned wiki, SCHEMA.md), frontmatter/wikilinks/index/log conventions, page-creation thresholds, health lint (orphans, broken links, contradictions, sha256 source drift).
- Target machine: AMD Ryzen AI Max+ 395 (Strix Halo, gfx1151), 119 GB RAM, working PyTorch ROCm 7.14 — the local-LLM option is real on this host.
- Bidirectional MCP: consume external MCP servers (stream pointers) AND expose the vault via an MCP endpoint (MoriNet already has MCP Phase 10 in progress — reuse rmcp experience).
- The name "SaussuNet": nod to Saussure (semiology: signifier/signified — a note as a sign within a system of relations) × knowledge network. Spelling: SaussuNet / crate `saussunet`.

## Constraints

- **Tech**: Rust, workspace `saussunet-core` (lib) + standalone web binary — reusable by MoriNet as a crate dependency
- **Format**: vault = standard Obsidian/llm-wiki (wikilinks, YAML frontmatter, SCHEMA.md/index.md/log.md) — no proprietary format
- **LLM**: abstract provider (trait-based), local or API — no hard dependency on a cloud vendor
- **Ontology**: optional, defaulting to an embedded llm-wiki ontology; YAML/OWL/RDF/schema formats
- **MCP sources**: never copy remote content into the vault — pointer notes only, with the access-point address
- **Continuity**: continuous source watching (poll WebDAV/Drive, MCP streams), not just one-shot builds
- **Preview**: mdbook CLI like MoriNet (present in PATH, LOUD failure if missing)
- **Deployment**: to our server (MoriNet `deploy.sh` pattern), but server config (hosts, paths, systemd credentials) lives in a **gitignored** file (e.g. `deploy.local.sh` / `.env.deploy`) — the repo only contains the example template

## Key Decisions

| Decision | Rationale | Outcome |
|----------|-----------|---------|
| Base = llm-wiki (Karpathy) + optional ontology (YAML/OWL/RDF) for classes/display/relations | The llm-wiki pattern already covers agentic vault building; the ontology adds semantics when the user needs it | — Pending |
| MCP both ways: consume (stream pointers, never copy) + expose the vault over MCP | MCPs are streams, not archives; the vault stays the local source of truth | — Pending |
| Abstract local/API LLM provider | Strix Halo makes local viable; cloud remains the comfort option | — Pending |
| Continuous source watching (poll + streams) rather than one-shot builds | The vault must "compound" continuously, like the llm-wiki log | — Pending |
| Full web UI (config, monitoring, review/edit) + MoriNet-style mdbook preview | Human review of generated notes is the quality guardrail of the AI pipeline | — Pending |
| Rust workspace: `saussunet-core` + standalone binary; MoriNet can depend on the lib | Dual standalone/embedded mode without code duplication or tight coupling | — Pending |
| Spelling corrected: SaussuNet / `saussunet` (not SaussuNoet) | User decision 2026-10-25 | ✓ Locked |
| Deploy to our server with gitignored server config (example template only in repo) | Hosts/paths/credentials must never be versioned; reuse MoriNet pattern without exposing holldemo | — Pending |
| All project artifacts written in English | User decision 2026-10-25 | ✓ Locked |

## Evolution

This document evolves at phase transitions and milestone boundaries.

**After each phase transition** (via `/gsd-transition`):
1. Requirements invalidated? → Move to Out of Scope with reason
2. Requirements validated? → Move to Validated with phase reference
3. New requirements emerged? → Add to Active
4. Decisions to log? → Add to Key Decisions
5. "What This Is" still accurate? → Update if drifted

**After each milestone** (via `/gsd:complete-milestone`):
1. Full review of all sections
2. Core Value check — still the right priority?
3. Audit Out of Scope — reasons still valid?
4. Update Context with current state

---
*Last updated: 2026-10-25 — translated to English per user request*
