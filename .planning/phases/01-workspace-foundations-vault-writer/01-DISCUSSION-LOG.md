# Phase 1: Workspace Foundations + Vault Writer - Discussion Log

> **Audit trail only.** Do not use as input to planning, research, or execution agents.
> Decisions are captured in CONTEXT.md — this log preserves the alternatives considered.

**Date:** 2026-10-25
**Phase:** 1-Workspace Foundations + Vault Writer
**Areas discussed:** Multi-vault schema shape, Human-edit conflict handling

---

## Gray areas offered for selection

| Option | Description | Selected |
|--------|-------------|----------|
| sqlx version pin | 0.9 now (migrate MoriNet later) vs 0.8 (align now) | |
| Multi-vault schema shape | vault_id column vs DB-per-vault | ✓ |
| Human-edit conflict handling | writer behavior on gen_hash mismatch; section definition | ✓ |
| Phase 1 operability | CLI subcommands vs minimal HTTP API | |

**Notes:** User selected two of four areas. sqlx pin and operability were left unselected and routed to Claude's discretion.

---

## Multi-vault schema shape

| Option | Description | Selected |
|--------|-------------|----------|
| One DB, vault_id column | Single pool + migration pass, matches VLET-04 wording | ✓ (via follow-up) |
| One DB file per vault | Hard isolation, per-vault copy/delete, multiplies pools/migrations | |
| One DB + thin VaultId abstraction | Store APIs keyed by VaultId so a later split is mechanical | |

**User's choice:** Free-text counter-question first — "do we need a DB when we have llm-wiki and md files?" — then selected the full-schema option below.
**Notes:** The DB-justification discussion (SQLite holds queue/watcher state/registries, never note content; `reindex` rebuilds) was accepted. User then chose to design the whole schema once rather than grow it per phase.

### Follow-up: how much SQLite belongs in Phase 1

| Option | Description | Selected |
|--------|-------------|----------|
| Minimal Phase 1 footprint | Only tables Phase 1 uses; queue/watcher tables arrive with their phases | |
| Full schema now | jobs, sync_state, entity_registry, page_registry designed once | ✓ |
| No SQLite in Phase 1 | Pure filesystem writer; DB in Phase 2 (deviates from INFR-02 / SC5) | |

### Filesystem path layout

| Option | Description | Selected |
|--------|-------------|----------|
| Paths keyed by vault id | `data/vaults/<vault_id>/` per vault | ✓ |
| Single configured path | vault_id only in schema until SRCX-04 | |

---

## Human-edit conflict handling

### Writer behavior on gen_hash mismatch (no review UI until Phase 5)

| Option | Description | Selected |
|--------|-------------|----------|
| Skip write + record conflict | `pending_proposals` row + `log.md` entry, waits for Phase 5 queue | ✓ |
| Sidecar proposal file | `.saussunet/proposals/<page>.md` beside the untouched human page | |
| Overwrite with backup | Keep timestamped copy of the human version | |

### Section definition and hash storage

| Option | Description | Selected |
|--------|-------------|----------|
| `##` headings, frontmatter hash map | section slug → hash in YAML frontmatter; frontmatter hashed separately | ✓ |
| Whole-page gen_hash | Any human edit blocks any regeneration until review | |
| Paragraph-level | Finer merge, noisy hashes, brittle slugs | |

### `protected: true` frontmatter flag

| Option | Description | Selected |
|--------|-------------|----------|
| Support protected flag | Writer refuses all writes; proposals still recorded as conflicts | ✓ |
| Not in v1 | gen_hash protection is enough | |

### Pages with no gen_hash map

| Option | Description | Selected |
|--------|-------------|----------|
| Treat as human-owned | Writer refuses to touch, logs the refusal | ✓ |
| Treat as new | Adopt page, record gen_hash from current content | |

**Notes:** Every branch was resolved toward non-destructive behavior — skip and record rather than clobber. The `pending_proposals` table is the handoff point to the Phase 5 review queue.

---

## Claude's Discretion

- **sqlx version pin (0.8 vs 0.9)** — planner decides using the research conflict table and Gaps section.
- **Phase 1 operability** (CLI vs minimal HTTP API to drive the writer) — planner decides; SC1 only requires that running the binary creates a vault.
- SCHEMA.md seed content, tag taxonomy defaults, `raw/` subdirectory conventions, index.md section layout — follow llm-wiki skill conventions.
- Error/logging library choice, test harness layout, CI shape.

## Deferred Ideas

- Review/merge UI consuming `pending_proposals` — Phase 5 (REVI-01).
- `jobs` / `sync_state` runtime behavior — Phases 2 and 4 (schema only in Phase 1).
- Google Drive / WebDAV / inbound MCP sources — Phase 4 and v1.x.
- Git auto-commit of the vault per batch (QUAL-01) — v2.
