# Feature Research

**Domain:** Knowledge-vault builders / llm-wiki compilers / automated PKM (sources → continuously maintained llm-wiki vault pipeline)
**Researched:** 2026-10-25
**Confidence:** MEDIUM-HIGH (HIGH on llm-wiki-compiler — full README analyzed; MEDIUM on the others — search snippets; base pattern comes from the local llm-wiki skill)

## Feature Landscape

Products analyzed: **llm-wiki-compiler / `llmwiki`** (atomicstrata, Node CLI, 2.2k ⭐ — the most mature direct competitor), **obsidian-second-brain** (Claude skills on an Obsidian vault), **llm-wiki-obsidian-blink** (vault template + TheSchema.md), **Basic Memory** (bidirectional markdown MCP), **Hermes llm-wiki skill** (agent-in-the-loop curation), **AuditToolbox Second Brain** (FTS5+LLM dedup), plus the "RAG vs wiki" literature (glukhov.org, affine.pro, llmwikis.org, criticism-and-mitigations).

Structural finding: the market is young (the Karpathy pattern dates from late 2025). llmwiki is the only one to have industrialized compile + review + lint + export + MCP, but it is a **one-shot CLI driven by the human** — no continuous monitoring, no inbound MCP sources, no standard ontology (OWL/RDF), read-only viewer. That is exactly SaussuNet's space.

### Table Stakes (Users Expect These)

| Feature | Why Expected | Complexity | Notes |
|---------|--------------|------------|-------|
| Multi-source ingestion (URL, PDF, files) | Every competitor does it (`llmwiki ingest`, `/obsidian-ingest`) | MEDIUM | Clean markdown extraction (readability, PDF→md); descriptive naming of raw/ files |
| YAML frontmatter on every page (title, created, updated, type, tags, sources) | llm-wiki convention; without it there is no Dataview, no lint, no staleness | LOW | Validated by every product in the domain |
| Immutable `raw/` + 3 layers (raw / wiki / SCHEMA.md) | The base pattern; "if the wiki is corrupted, rebuild it from raw/" | LOW | Forbid any pipeline writes into raw/ |
| Page-creation threshold (2+ sources OR central in 1 source) | Prevents page explosion; SCHEMA.md of the llm-wiki skill | MEDIUM | Two phases (concept extraction then page generation, like llmwiki) for stable merging |
| Dedup on add (duplicate detection before page creation) | AuditToolbox: FTS shortlist + LLM judgment; otherwise the wiki "fragments" instead of compounding | MEDIUM | Mandatory orientation step: read index.md + search before creating (anti-duplicate) |
| Source drift via sha256 | llm-wiki skill: body hash, recomputed on re-ingest → skip if identical, flag drift if different | LOW | The guardrail of continuous monitoring: without it, the permanent poll recycles in an endless loop |
| Cross-linking: min 2 outgoing wikilinks per page + backlinks | "Orphan pages are invisible" — every implementation | MEDIUM | Lint must check both directions (outgoing AND incoming/orphans) |
| index.md + log.md maintained on every action | Navigational backbone; append-only log with rotation | LOW | Rotating log (500 entries), page count in index |
| Health lint (orphans, broken links, incomplete index, frontmatter, tags outside taxonomy, size >200 lines, sha256 drift) | `llmwiki lint`, `/obsidian-health`, `/brain-fix` — everyone has it; "slow entropy" is the domain's #1 failure mode | MEDIUM | Reports grouped by severity; schedulable (cron) + on-demand triggerable |
| Provenance markers (`sources:` in frontmatter, `^[raw/...]` on 3+ source syntheses) | Without traceability, an ingestion-time hallucination becomes durable truth cited by future queries | MEDIUM | llmwiki cites down to line ranges; lint validates citations |
| Trust signals (`confidence`, `contested`, `contradictions:`) | Recommended frontmatter of the pattern; lint must surface weak pages for review | LOW | Never mark `high` without multi-sourcing |
| Contradiction handling: flag + update policy (dates, sourcing, both sides' positions) | Karpathy baseline: "note both positions, mark in frontmatter, flag for review" | MEDIUM | See anti-feature "100% automatic resolution" |
| Incremental compilation (only changed sources go back through the LLM) | llmwiki: "incremental rebuilds so only changed sources hit the model" — the economic condition for local LLM | MEDIUM | Requires a state index (SQLite) source→derived pages |
| Human review queue before publication | llmwiki 1.4 "review answers before publishing"; llmwikis.org: destructive operations (merge/move/archive) are NEVER auto-published | HIGH | This is the quality guardrail claimed by PROJECT.md; see Dependencies |
| Obsidian compatibility (wikilinks, frontmatter, graph view) | Requirement of the project + of all competitors | LOW | No proprietary extensions |
| MCP exposure of the vault | llmwiki `serve` (ingest/compile/query/lint/status/context); MoriNet expects an MCP endpoint | MEDIUM | Reuse rmcp (MoriNet Phase 10 experience) |
| Vault versioning (git) | Documented Obsidian+git pattern: clean baseline, commits per batch, diff = review surface | MEDIUM | Auto-commit per pipeline batch with rich message (N pages created/modified); optional but expected |

### Differentiators (Competitive Advantage)

| Feature | Value Proposition | Complexity | Notes |
|---------|-------------------|------------|-------|
| Continuous source monitoring (Drive/WebDAV poll + MCP streams) | No competitor does it continuously: llmwiki is a human-triggered CLI, second-brain is cron skills. This is THE core value of SaussuNet | HIGH | Per-source detectors (etag/mtime/Drive revision, WebDAV poll) → work queue → pipeline; one job at a time on the vault (beaussan.io pattern) |
| Inbound MCP sources as pointer notes (never a copy) | No product consumes MCPs as live sources; "MCPs are streams, not archives" | HIGH | `pointer` page type with the access point's address + content read on demand; potential conflict with classic lint (broken link ≠ broken page) |
| Optional YAML/OWL/RDF ontology | llmwiki has proprietary JSON profiles (CLP) but no standard formats; SaussuNet speaks semantic-web languages when the user needs them | HIGH | Ontology = extension of SCHEMA.md (classes, typed relations, display); default = embedded llm-wiki ontology |
| Bidirectional MCP (consume + expose) | llmwiki only exposes; Basic Memory only has the conversational read/write side | MEDIUM | Both sides share the vault abstraction; capitalize on rmcp |
| Web review/edit UI + pipeline tracking | llmwiki viewer = read-only; second-brain = zero UI. A real review queue (before/after diff, approve/reject batch) is rare and it is the promised guardrail | HIGH | Pre/post-run snapshots → diff → review (AI-ownership pattern); SQLite pipeline state |
| Embeddable library (`saussunet-core`) + standalone binary | llmwiki has a TS SDK but no server integration story; MoriNet will depend on the crate without duplicating | MEDIUM | All logic outside HTTP/CLI lives in the lib; the binary is just a façade |
| mdbook preview of the vault | No competitor does a static preview; reuses the MoriNet pattern | LOW | mdbook CLI, LOUD failure if absent |
| Abstract local/API LLM provider | llmwiki supports many providers but SaussuNet targets local-first on Strix Halo (ROCm) — comfort + cost + privacy | MEDIUM | Rust trait + default impl (OpenAI-compatible API) + local impl (Ollama/llama.cpp) |
| Freshness repair (`refresh --stale`) | llmwiki: repairs outdated pages without recompiling new sources; beyond plain lint | MEDIUM | Requires the incremental state index + source dates |
| Multi-vaults (N vaults, one server) | MoriNet already serves multi-vaults; llmwiki is single-project. Each vault = its own SCHEMA/ontology/sources | MEDIUM | Isolate SQLite state + config + queue per vault; decide early (touches the config schema) |
| Export/MoriNet integration | The vault is the artifact; MoriNet serves it. JSON/JSONL exchange + MCP suffice — no proprietary format | LOW | llmwiki exports OKF/JSON-LD/GraphML — good ideas to pillage, not to clone entirely |

### Anti-Features (Commonly Requested, Often Problematic)

| Feature | Why Requested | Why Problematic | Alternative |
|---------|---------------|-----------------|-------------|
| Automatic contradiction resolution without review | obsidian-second-brain sells it (`/obsidian-reconcile` "resolves them automatically") | A false reconciliation settles in as truth and propagates through links; "a bad wiki page affects many future queries" (glukhov) | `contested` flag + dated/sourced resolution proposal in the review queue; the human decides |
| Auto-publish with no review gate at all | "no manual writing" read as "no intervention" | Ingestion-time hallucination is the pattern's #1 documented risk; generated structure = manufactured authority | Auto-publish allowed only for minor updates (diff thresholds); creation/merge/archive/split always go through review |
| Filling the vault with a page for every mention | The LLM makes page creation free, hence tempting | Empty taxonomy, shallow pages, meaningless links, "false completeness" | Strict 2+ sources threshold + lint that flags pages never linked/updated |
| Freeform tags (outside taxonomy) | Apparent flexibility | "Freeform tags decay into noise" — unrecoverable tag sprawl | Taxonomy in SCHEMA.md, lint rejects any undeclared tag, adding a tag = modify SCHEMA first |
| Vector search/embeddings as the core of retrieval | The RAG reflex; llmwiki does semantic+BM25 hybrid | The wiki pattern lives on index-first + links; embeddings = heavy dependency (embedding model, index to maintain) for zero gain under ~hundreds of pages | v1: index.md + full-text search (SQLite FTS) + wikilink graph; embeddings only if usage proves the need |
| Copying remote MCP content into the vault | "Just put everything in raw/" | Violates the project constraint; the vault lies about what is live; inevitable drift; mass of duplicated data | Pointer notes + stream read on demand; lint treats pointer links (validity = reachability) separately from internal links |
| Real-time collaborative editing / multi-account | The Notion reflex | Already out of scope; merge conflicts between human and pipeline = nightmare without CRDTs | Review queue + "one job at a time" lock; git for history |
| Signed template ecosystem / marketplace (Ed25519, taps, package revocation) | llmwiki 1.1 built it | Distribution complexity with no purpose for a personal self-hosted server | Versioned config/ontology files, nothing more |
| Viewer themes / pretty static site generator | llmwiki sells themes (Scientific Clay, Nebula…) | That is not the value; mdbook covers the preview | Standard mdbook, minimal styling |
| Obsidian Sync / obsidian-headless integrated into the product | "I want my notes on my phone" expectation | External sync chain (Node, paid Obsidian account) coupled to the server; outside core value | Document the external recipe (obsidian-git or Syncthing); do not build it |

## Feature Dependencies

```
[Continuous monitoring (poll/streams)]
    └──requires──> [Dedup + sha256 drift]
                       └──requires──> [Ingestion + raw/ + source frontmatter]
[Incremental compilation]
    └──requires──> [SQLite state index (source→pages)]
                       └──requires──> [Dedup + sha256 drift]
[Creation thresholds + page dedup]
    └──requires──> [Orientation via index.md + FTS search]
[Cross-linking min 2 wikilinks]
    └──requires──> [Creation thresholds]  (only link pages that deserve to exist)
[Contradictions (flag + confidence)]
    └──requires──> [Sourced provenance] ──requires──> [Ingestion]
[Human review queue (UI)]
    └──requires──> [Pipeline tracking (state, before/after diffs)]
    └──enhances───> [Contradictions], [Cross-linking]  (where the human decides)
[Lint/health]
    └──requires──> [Frontmatter + wikilinks + index.md]
[Freshness repair (refresh --stale)]
    └──requires──> [Incremental compilation] ──requires──> [Freshness lint]
[MCP pointer sources]
    └──requires──> [MCP client + pointer page type]
    └──enhances───> [Continuous monitoring]
[Outbound MCP exposure]
    └──requires──> [Vault + index] (independent of the pipeline)
[YAML/OWL/RDF ontology]
    └──extends───> [SCHEMA.md] ; ──requires──> [Taxonomy/relation lint]
[Git versioning of the vault]
    └──enhances───> [Review queue] (the git diff = review surface)
[Multi-vaults]
    └──conflicts──> [Monolithic config/state] — to settle BEFORE freezing the SQLite schema
[mdbook preview] — independent (consumes the vault on output)

[100% auto-publish] ──conflicts──> [Human review queue]
[Freeform tags] ──conflicts──> [SCHEMA taxonomy + lint]
```

### Dependency Notes

- **Continuous monitoring requires dedup/sha256 drift:** without content hashing, every poll reprocesses everything and the local LLM spins its wheels; sha256 is the keystone of the entire continuous regime (skip, drift, incremental).
- **Incremental compilation requires the state index:** knowing which pages to regenerate when a source changes requires the source→pages map (SQLite, same database as pipeline tracking).
- **Review queue requires before/after diffs:** useful review shows a diff, not a final page — pre/post-run snapshot (AI-ownership pattern) or git.
- **Contradictions require provenance:** you can only date/source two positions if every claim traces back to a raw/ file with an ingestion date.
- **MCP pointer sources enhance continuous monitoring:** MCPs bring live data without polling; but lint must treat pointers separately (their "freshness" = reachability of the remote server, not a hash).
- **Ontology extends SCHEMA.md:** the ontology does not replace SCHEMA, it specializes it (classes → page types, relations → typed wikilink types à la Basic Memory `depends_on [[X]]`). Building it before the taxonomy lint would leave it unverified.
- **Multi-vaults conflicts with monolithic state:** adding multi-vaults after the fact touches config, SQLite schema, paths, queue — an architecture decision to make in phase 1 even if only one vault ships.
- **Auto-publish conflicts with review queue:** reconcilable via a per-type policy (minor update auto / creation-merge-archive in review), not via a single global toggle.

## MVP Definition

### Launch With (v1)

- [ ] Web/PDF ingestion pipeline → raw/ + pages (frontmatter, wikilinks, index.md, log.md) — this is the core value; without nothing else, we have nothing
- [ ] Dedup + sha256 drift + page-creation thresholds — without them the vault drifts into a junk dump
- [ ] Continuous monitoring on ONE file source (WebDAV or Drive) — validate the continuous regime, not all 4 connectors
- [ ] Incremental compilation (SQLite state index) — the cost condition for local LLM
- [ ] Health lint (broken links, orphans, index, frontmatter, taxonomy, drift) — the anti-entropy guardrail
- [ ] Contradictions flagged (`contested`, `contradictions:`) + provenance — never automatic resolution in v1
- [ ] Minimal review queue (UI: list of pending pages, diff, approve/reject) + pipeline tracking (ingestion, errors)
- [ ] Abstract LLM provider (1 API + 1 local)
- [ ] Outbound MCP exposure (read-only: index, read, search, lint) — prerequisite for MoriNet integration
- [ ] mdbook preview
- [ ] Multi-vault decision made (schema ready), one vault shipped

### Add After Validation (v1.x)

- [ ] MCP pointer sources — after the file pipeline is validated; touches lint (pointers) and the MCP client
- [ ] Second/third connectors (Drive if WebDAV first, etc.)
- [ ] Freshness repair (`refresh --stale`) — trigger: lint surfaces too many stale pages to fix by hand
- [ ] YAML ontology (classes/typed relations) — trigger: the default llm-wiki ontology is no longer enough
- [ ] Auto-commit git per pipeline batch — trigger: need rollback/history of regenerations
- [ ] Multi-vaults actually enabled — trigger: a real second vault
- [ ] Numeric eval/health (score, per-page distribution, citation coverage) — trigger: doubting quality without numbers

### Future Consideration (v2+)

- [ ] OWL/RDF (ontology import/export) — niche audience, high cost, YAML covers personal usage
- [ ] MoriNet library integration (`saussunet-core` crate as a dependency) — MoriNet must first consume the MCP
- [ ] Hybrid embeddings+BM25 search — only if FTS+index saturates (corpus > ~500 pages)
- [ ] Assisted contradiction resolution (auto proposal + validation) — never without a battle-tested review queue
- [ ] Compounding queries (query --save in queries/) — the pattern exists, add it once the MCP query is being used

## Feature Prioritization Matrix

| Feature | User Value | Implementation Cost | Priority |
|---------|------------|---------------------|----------|
| Ingestion pipeline + llm-wiki conventions | HIGH | MEDIUM | P1 |
| Dedup/sha256 drift + page thresholds | HIGH | MEDIUM | P1 |
| Continuous monitoring (1 connector) | HIGH | MEDIUM | P1 |
| Incremental compilation | HIGH | MEDIUM | P1 |
| Health lint | HIGH | MEDIUM | P1 |
| Contradiction flag + provenance | HIGH | MEDIUM | P1 |
| Review queue + pipeline tracking (UI) | HIGH | HIGH | P1 |
| Abstract LLM provider | HIGH | MEDIUM | P1 |
| Outbound MCP exposure | MEDIUM | MEDIUM | P1 |
| mdbook preview | MEDIUM | LOW | P1 |
| MCP pointer sources | HIGH | HIGH | P2 |
| Freshness repair | MEDIUM | MEDIUM | P2 |
| YAML ontology | MEDIUM | MEDIUM | P2 |
| Auto git versioning | MEDIUM | LOW | P2 |
| Multi-vaults | MEDIUM | MEDIUM | P2 |
| Numeric eval/health | MEDIUM | MEDIUM | P2 |
| OWL/RDF ontology | LOW | HIGH | P3 |
| MoriNet lib integration | MEDIUM | MEDIUM | P3 |
| Hybrid embeddings search | LOW | HIGH | P3 |

**Priority key:**
- P1: Must have for launch
- P2: Should have, add when possible
- P3: Nice to have, future consideration

## Competitor Feature Analysis

| Feature | llmwiki (atomicstrata) | obsidian-second-brain | Basic Memory | Our Approach (SaussuNet) |
|---------|------------------------|----------------------|--------------|--------------------------|
| Ingestion | CLI `ingest` URLs/files, recursive folders, exclusions | `/obsidian-ingest` (skill), URLs/PDF/audio/screenshots, 5-15 pages touched/source | Via LLM conversation (MCP), no connectors | Server with continuously monitored connectors (Drive, WebDAV, web/PDF, MCP) |
| Dedup / thresholds | 2-phase extract→generate, concept merging, optional `--no-pages-limit` | Rewriting existing pages rather than adding | Entities per permalink, basic dedup | 2+ sources threshold + FTS shortlist + LLM judgment, 2-phase |
| Source drift | Freshness badges, `lint`, `refresh --stale` | Freshness "every fact timeless, dated, or a pointer" | No | sha256 on raw/ + native polling of sources |
| Contradictions | Review policy auto-hold on contradiction/confidence | AUTO resolution (`/obsidian-reconcile`) — anti-feature for us | No | Flag + review queue, human resolution (assisted in v2) |
| Human review | CLI/SDK review queue, batch approve, gates on write path | None (autonomous cron agents) | The human edits the files | Dedicated web UI: before/after diff, approve/reject, pipeline tracking |
| Lint/health | `lint` + `eval` (score, citation coverage) | `/obsidian-health` (typed-edge lint, taxonomy audit) | No | Full lint + schedulable; eval in v1.x |
| Versioning | No (FS only) | External Obsidian-git | "track with git" (manual) | Optional auto-commit per pipeline batch |
| MCP | Outbound only (`serve`) | No | Bidirectional but conversational (read/write) | Bidirectional: inbound as stream pointers (never a copy) + outbound |
| Ontology | CLP profile.json proprietary (typed entities/relations/workflows) | Tag taxonomy + typed relations in notes | Typed relations in wikilinks (`depends_on [[X]]`) | SCHEMA.md + optional YAML → OWL/RDF ontology (standards) |
| UI | Read-only viewer + themes | None | Cloud app (not local-first) | Web UI for config + pipeline + review/edit + mdbook preview |
| Multi-vaults | Single project | Single vault | Multi-projects (cloud) | Native multi-vaults from the schema (parity with MoriNet) |
| Format | Markdown + `.llmwiki/` state | Markdown + skills | Markdown + SQLite | Standard Markdown/llm-wiki + SQLite (state only), zero proprietary content format |

## Sources

- llm-wiki-compiler (atomicstrata/llm-wiki-compiler, README v1.5.0, Oct. 2026) — https://github.com/atomicstrata/llm-wiki-compiler
- Karpathy, "LLM Wiki" gist — https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f
- Local llm-wiki skill (Hermes) — /home/c/.hermes/profiles/devo/skills/research/llm-wiki/SKILL.md
- obsidian-second-brain (eugeniughelbur) — https://github.com/eugeniughelbur/obsidian-second-brain
- llm-wiki-obsidian-blink — https://www.sourcepulse.org/projects/28747593
- Basic Memory (knowledge format, MCP) — https://docs.basicmemory.com/concepts/knowledge-format
- AuditToolbox Second Brain (FTS5+LLM dedup, /brain-fix) — https://www.audittoolbox.com/tools/brain/
- "LLM Wiki — Compiled Knowledge That RAG Cannot Replace" (risks, review queue) — https://www.glukhov.org/knowledge-management/knowledge-systems-architectures/compiled-knowledge/what-is-llm-wiki/
- "LLM-Wiki criticism and mitigation map" (po4yka/llm-wiki-skills) — https://github.com/po4yka/llm-wiki-skills/blob/main/skills/llm-wiki-human-first-design/references/docs/criticism-and-mitigations.md
- Affine, "Inside the LLM Wiki Movement" (failure patterns, git history, review by dependency) — https://affine.pro/blog/llm-wiki
- llmwikis.org, "Improve an Existing Wiki" (change control: what can be automated vs human) — https://llmwikis.org/guide/improve-existing-wiki/
- Obsidian + Git workflow for agents (clean baseline, diff budget, gates) — https://www.learnprompt.pro/en/obsidian-ai/obsidian-git-workflow/
- "Using AI in Obsidian Without Giving Up Control" (pre/post-run snapshots) — https://beaussan.io/blog/ai-obsidian-with-ownership/
- Guide breakdown Karpathy LLM Wiki (scale limits ~100 sources, compounded errors) — https://nandigamharikrishna.substack.com/p/andrej-karpathys-llm-wiki-full-breakdown

---
*Feature research for: knowledge-vault builders / llm-wiki compilers / automated PKM*
*Researched: 2026-10-25*
