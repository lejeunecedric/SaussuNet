# Phase 1: Workspace Foundations + Vault Writer - Context

**Gathered:** 2026-10-25
**Status:** Ready for planning

<domain>
## Phase Boundary

A self-hosted Rust binary owns a standards-compliant llm-wiki vault through a single atomic writer, with human edits protected and the schema ready for many vaults. Delivered: Cargo workspace (`crates/saussunet-core` lib + `crates/saussunet` binary), SQLite store (WAL, full schema designed once, every table carries `vault_id`), vault initialization (SCHEMA.md, index.md, log.md, immutable `raw/`), page writing with full YAML frontmatter and ≥2 outbound wikilinks, atomic temp+rename writes via a single writer, and per-section `gen_hash` three-way-merge protection. No LLM, no connectors, no UI in this phase.

Requirements: INFR-01, INFR-02, VLET-01, VLET-02, VLET-03, VLET-04.

</domain>

<decisions>
## Implementation Decisions

### Database role & philosophy
- **D-01:** SQLite never stores note content. Vault FS is the sole source of truth; SQLite holds only derived/durable state (registries, and later queues/watcher state) and is fully rebuildable via a `reindex` operation that scans the vault. Losing the DB file costs zero content.
- **D-02:** Create the FULL schema in Phase 1 migrations — `vaults`, `page_registry`, `entity_registry`, `jobs`, `sync_state`, `pending_proposals` — so migrations are designed once instead of reshaped per phase. Tables that Phase 1 does not exercise (`jobs`, `sync_state`) exist as schema + minimal store APIs; their behavior lands in their own phases.

### Multi-vault readiness
- **D-03:** One SQLite DB (`data/saussunet.db`), every table carries a `vault_id` column. No per-vault DB files.
- **D-04:** Filesystem paths are keyed by vault id: `data/vaults/<vault_id>/` holds each vault (clean directory story for Obsidian and for MoriNet serving later). v1 ships exactly one vault, but config, schema, paths, and store APIs all carry the id (VLET-04).

### Human-edit protection (gen_hash)
- **D-05:** Sections are Markdown `##` headings. Per-section `gen_hash` map lives in the YAML frontmatter (section slug → hash); the frontmatter itself is hashed separately from section bodies.
- **D-06:** On `gen_hash` mismatch (proof of a human edit), the writer SKIPS the write, records the proposal as a conflict row in `pending_proposals` (SQLite), and appends a `log.md` entry. The conflict waits for the Phase 5 review queue — no silent overwrite, ever.
- **D-07:** Pages may carry `protected: true` in frontmatter (user-adopted pages). The writer refuses ALL writes to protected pages; proposals against them are still recorded as conflicts.
- **D-08:** A page in `entities/`/`concepts/` with NO gen_hash map is human-owned: the writer refuses to touch it and logs the refusal. The writer only ever overwrites pages whose gen_hash it wrote itself (first-write provenance).

### Claude's Discretion
- **sqlx version pin (0.8 vs 0.9):** user did not select this area for discussion — planner decides, guided by research (SUMMARY.md conflict table + Gaps: two sqlx majors cannot coexist in one binary when `saussunet-core` is later embedded in MoriNet; MoriNet is on 0.8; MoriNet lib integration is QUAL-05/v2).
- **Phase 1 operability** (how a human drives the vault writer before the Phase 5 UI — CLI subcommands vs minimal HTTP API): planner decides; roadmap SC1 only requires that "running the binary creates a vault".
- SCHEMA.md seed content, tag taxonomy defaults, `raw/` subdirectory conventions, index.md section layout: follow the llm-wiki skill conventions (canonical ref below).
- Error/logging library, test harness layout, CI shape: standard Rust patterns (thiserror/anyhow, `tests/` with temp vault fixtures per ARCHITECTURE.md).

</decisions>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Project planning (repo)
- `.planning/PROJECT.md` — constraints (workspace split, llm-wiki format, no proprietary format), key decisions
- `.planning/REQUIREMENTS.md` — VLET-01..04, INFR-01/02 definitions and Out-of-Scope table
- `.planning/ROADMAP.md` §"Phase 1" — the 5 success criteria this phase must make TRUE

### Research (repo)
- `.planning/research/ARCHITECTURE.md` — recommended workspace layout (`crates/saussunet-core` zero-axum, `migrations/` at root), Pattern 3 (FS = truth, SQLite = rebuildable projection), Persistence table with suggested key tables, Anti-Patterns 1/2/6
- `.planning/research/PITFALLS.md` — Pitfall 4 (feedback loop: only `raw/` is evidence; watchers refuse vault-internal paths), Pitfall 6 (single-writer + atomic writes, retrofit = rewrite), Pitfall 7 (gen_hash from first write, `protected: true` origin); "Looks Done But Isn't" items for concurrent writes and human-edit protection
- `.planning/research/STACK.md` — pinned crate versions (tokio 1.54, axum 0.8.9, sqlx 0.9 flag, serde_yml 0.0.13, pulldown-cmark 0.13, gray_matter 0.3, clap 4.6)
- `.planning/research/SUMMARY.md` — "Gaps to Address" (sqlx alignment, multi-vault schema shape — both resolved here) and Phase 1 rationale

### Format authority (outside repo)
- `/home/c/.hermes/profiles/devo/skills/research/llm-wiki/SKILL.md` — llm-wiki conventions: 3 layers, frontmatter fields, wikilinks, index.md/log.md rules, log rotation (~500 entries → `log-YYYY.md`), SCHEMA.md role. Vault output MUST conform.
- MoriNet reference implementation at `/home/c/Projects/Dev/MoriNet` — sibling patterns: sqlx migrations layout, WAL setup, config.toml pattern (NOT a dependency; reference only)

</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- Greenfield repo — only `HERMES.md` exists. Nothing to reuse in-repo.
- MoriNet (`/home/c/Projects/Dev/MoriNet`) as pattern donor: sqlx migration wiring, SQLite WAL pragmas, config.toml loading, mdbook-subprocess philosophy (loud failure).

### Established Patterns
- Two-member Cargo workspace with `migrations/` at workspace root shared by lib and bin (ARCHITECTURE.md structure).
- `saussunet-core` has ZERO axum/HTTP-server dependency — CI-reviewable boundary (Anti-Pattern 1).
- Single serialized vault-writer task; all mutations funnel through it; atomic write-temp+rename in the same directory.
- Vault FS = truth; SQLite = rebuildable projection (`reindex`).

### Integration Points
- Binary `main.rs`: clap config load → open SQLite (WAL) → ensure vault dir exists → vault init (SCHEMA/index/log/raw) → writer task wiring.
- Store APIs keyed by `VaultId` from day one (D-03/D-04).

</code_context>

<specifics>
## Specific Ideas

- User challenged the DB's existence ("do we need a DB when we have llm-wiki and md files?") and accepted the projection/queue rationale — keep SQLite strictly minimal in PURPOSE even though the full schema is created up front: no note content in SQLite, ever.
- Conflict handling is deliberately non-destructive at every branch (skip + record, never clobber) — the review UI in Phase 5 consumes what Phase 1 records.

</specifics>

<deferred>
## Deferred Ideas

- Review/merge UI for `pending_proposals` conflicts — Phase 5 (REVI-01).
- `jobs` / `sync_state` table behavior (atomic claim, retry, etags) — Phase 2/4; Phase 1 only creates the schema shape.
- Google Drive / WebDAV / MCP sources — Phases 4+ (v1.x for Drive/MCP per REQUIREMENTS v2 section).
- Git auto-commit of vault per batch (Pitfall 7 recovery aid, QUAL-01) — v2.

</deferred>

---

*Phase: 1-Workspace Foundations + Vault Writer*
*Context gathered: 2026-10-25*
