---
gsd_state_version: 1.0
milestone: v1.0
milestone_name: milestone
status: ready_to_plan
last_updated: "2026-10-25T21:12:23.816Z"
progress:
  total_phases: 6
  completed_phases: 1
  total_plans: 4
  completed_plans: 4
  percent: 17
---

# State: SaussuNet

**Project:** [PROJECT.md](PROJECT.md)
**Roadmap:** [ROADMAP.md](ROADMAP.md)
**Requirements:** [REQUIREMENTS.md](REQUIREMENTS.md)

## Core Value

Feed raw sources (Drive, WebDAV, web, MCP) and get, continuously, an up-to-date, interlinked, structured llm-wiki vault — with no manual note writing.

## Current Focus

**Phase 1: Workspace Foundations + Vault Writer** (Not started)

Single-writer vault layer, atomic writes, `gen_hash` edit protection, and the multi-vault-ready SQLite schema — the pieces that cannot be retrofitted (see research PITFALLS 4, 6, 7).

## Project Ref

- **Mode:** mvp (Vertical MVP — every phase ships an end-to-end user capability)
- **Granularity:** standard (6 phases derived from 25 v1 requirements)
- **Milestone:** v1.0 (planned)

## Decisions

(None yet — decisions are logged here as phases plan and execute.)

## Phase History

| Phase | Status | Key Outcomes |
|-------|--------|--------------|
| 1. Workspace Foundations + Vault Writer | Not started | — |
| 2. LLM Provider + Durable Job Queue | Not started | — |
| 3. Extraction Pipeline | Not started | — |
| 4. Continuous Source Watching | Not started | — |
| 5. Health Lint + Review & Monitoring UI | Not started | — |
| 6. Exposure + Deployment | Not started | — |

## Open Items

- sqlx version alignment with MoriNet (0.8 vs 0.9) — force the decision in Phase 1 planning (research Gaps).
- Multi-vault schema shape — force the decision in Phase 1 even though one vault ships.
- pdf_oxide pass-rate is vendor-sourced — validate on a real PDF corpus in Phase 3.

---
*Last updated: 2026-10-25 — roadmap created (6 phases, 25/25 v1 requirements mapped)*
