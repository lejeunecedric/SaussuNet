# Phase 3: Extraction Pipeline - Discussion Log

> **Audit trail only.** Do not use as input to planning, research, or execution agents.
> Decisions are captured in CONTEXT.md — this log preserves the alternatives considered.

**Date:** 2026-10-29
**Phase:** 3-Extraction Pipeline
**Areas discussed:** Ingestion entry & job flow, Text extraction, LLM extraction design, Thresholds & dedup

---

## Ingestion entry & job flow

| Option | Description | Selected |
|--------|-------------|----------|
| CLI `saussunet ingest` inserts a job; daemon drains | Reuses Phase 2 queue, crash-safe | |
| CLI processes synchronously inline | No daemon needed | |
| Minimal HTTP endpoint POST /ingest | Daemon serves ingestion | ✓ |

| Option | Description | Selected |
|--------|-------------|----------|
| One job per source | fetch→extract→LLM→writes in one handler | ✓ |
| Job chain | fetch job enqueues per-page jobs | |

| Option | Description | Selected |
|--------|-------------|----------|
| Fetch inside the job at execution time | Queue retry/backoff covers network | ✓ |
| Fetch at submit time | | |

| Option | Description | Selected |
|--------|-------------|----------|
| Copy PDF bytes into raw/ at submit | sha256 from the copy, vault self-contained | ✓ |
| Reference the path, re-read each run | | |

**User's choice:** HTTP endpoint entry (against recommendation — respected); recommendations on the rest.
**Notes:** Daemon becomes the primary surface from Phase 3 on; endpoint must honor the configurable `/saussunet` base path (PROJECT.md decision).

---

## Text extraction

| Option | Description | Selected |
|--------|-------------|----------|
| readability + htmd crates | Article-body detection → clean Markdown, pure Rust | ✓ |
| Naive tag stripping | Keeps nav/boilerplate noise | |
| External reader API (jina.ai-style) | Adds network dependency | |

| Option | Description | Selected |
|--------|-------------|----------|
| pdftotext subprocess (poppler), LOUD failure | MoriNet mdbook pattern, best quality | ✓ |
| Pure-Rust pdf-extract | No system dep, weaker layouts | |
| pdf-extract then pdftotext fallback | | |

| Option | Description | Selected |
|--------|-------------|----------|
| raw/YYYY-MM-DD-slug.md + sha256 frontmatter | Readable provenance markers, Obsidian-friendly | ✓ |
| raw/<sha256>.md content-hash names | Unreadable provenance | |

| Option | Description | Selected |
|--------|-------------|----------|
| Auto re-ingest + log.md drift entry + normal merge | Vault stays current, nothing silent | ✓ |
| Flag only, human triggers re-ingest | | |
| Auto re-ingest + mark pages contested | | |

**User's choice:** "1a 2a 3a 4a" (all recommendations), answered as plain text after two interactive-form timeouts.

---

## LLM extraction design

| Option | Description | Selected |
|--------|-------------|----------|
| Two passes: candidates, then per-page generate/merge | Pass 2 sees existing page + new source | ✓ |
| Single call | Candidates + finished bodies at once | |
| Pass 1 + one batched pass-2 call | Fewer calls, bigger blast radius | |

| Option | Description | Selected |
|--------|-------------|----------|
| LLM judges centrality per candidate (central + justification) | Semantic judgment in typed schema | ✓ |
| Programmatic mention counting | Brittle | |
| Hybrid score + configurable threshold | | |

| Option | Description | Selected |
|--------|-------------|----------|
| Pass-2 LLM writes ^[raw/...] per claim; validator enforces | Claim-level provenance, deterministic enforcement | ✓ |
| Programmatic per-section stamping | Coarse but guaranteed | |

| Option | Description | Selected |
|--------|-------------|----------|
| Pass-2 merge detects contradictions, keeps both, sets contested | Matches PIPE-05, non-destructive | ✓ |
| Deterministic diff flags rewrites | Noisy | |
| Defer to Phase 5 lint | Leaves PIPE-05 open | |

| Option | Description | Selected |
|--------|-------------|----------|
| Endpoint base-path-aware from day one | No Phase 5 retrofit | ✓ |
| Plain /ingest now, retrofit later | | |

**User's choice:** "1 a 2 a 3 a 4 a 5 a" (all recommendations).

---

## Thresholds & dedup

| Option | Description | Selected |
|--------|-------------|----------|
| config.toml [pipeline] min_sources=2 | Operational knob, existing config pattern | ✓ |
| SCHEMA.md in-vault declaration | Format authority, not runtime config | |
| Hardcoded constants | | |

| Option | Description | Selected |
|--------|-------------|----------|
| Deterministic normalize + exact/alias match | Predictable, zero extra LLM cost | ✓ |
| Deterministic + LLM resolver for near-misses | Extra call per ambiguous name | |
| Exact match only | Trivial variants slip through | |

| Option | Description | Selected |
|--------|-------------|----------|
| Pass-1 LLM proposes aliases; registry merges | Registry improves per source | ✓ |
| Canonical names only, manual aliases | | |

| Option | Description | Selected |
|--------|-------------|----------|
| One source = distinct raw/ record; drift replaces, no double-count | Matches sha256 no-op semantics | ✓ |
| One source = distinct URL | | |

**User's choice:** "ok?" — interpreted and confirmed as all-recommended (1a 2a 3a 4a).

---

## Claude's Discretion

- Validator hook point (extend `write_page` vs pre-write module) — must stay deterministic, reject before disk.
- Job `kind` strings + payload schema + worker kind-dispatch shape.
- Pass-1/pass-2 prompt texts and typed JSON schemas.
- Module layout in core.
- Axum router shape in the binary crate (minimal: POST /ingest + health).
- MockProvider scripting arrangement for two-pass tests.

## Deferred Ideas

- CLI `saussunet ingest` submitter (HTTP chosen instead; Phase 5 UI is the real surface)
- Fuzzy / LLM-assisted entity resolution for near-misses
- Ingestion/contested surfacing in web UI — Phase 5
- Full-vault drift sweep + lint — Phase 5
- Source watching/polling — Phase 4
- Confidence scoring beyond `contested` boolean
