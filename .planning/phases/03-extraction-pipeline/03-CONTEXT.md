# Phase 3: Extraction Pipeline - Context

**Gathered:** 2026-10-29
**Status:** Ready for planning

<domain>
## Phase Boundary

Submitting a web URL or PDF produces entity and concept pages in the vault: text is extracted and stored under `raw/` (source_url/ingested/sha256 frontmatter), the LLM extracts entity/concept candidates, threshold-gated pages are created or updated with provenance markers, dedup is enforced by the entity registry (resolve-before-create), and every page passes a deterministic write-path validator before touching disk — the LLM never writes to the vault directly. Unchanged sources are a sha256 no-op; changed sources are re-ingested with a drift log entry.

No source watching/polling (Phase 4), no review/monitoring UI (Phase 5), no lint (Phase 5), no MCP exposure (Phase 6).

Requirements: PIPE-01, PIPE-02, PIPE-03, PIPE-04, PIPE-05, PIPE-06.

</domain>

<decisions>
## Implementation Decisions

### Ingestion entry & job flow
- **D-01:** Entry point is a minimal HTTP endpoint `POST /ingest` on the daemon (axum in the **binary crate only** — `saussunet-core` stays zero-axum per the CI guard). Accepts JSON `{"url": ...}` or a PDF file upload; inserts a durable job and returns the job id. A CLI submitter is deferred (Phase 5 UI supersedes).
- **D-02:** One job per source: fetch → extract text → LLM passes → page writes all inside one job handler. Simple retry semantics; a crash resumes the whole source.
- **D-03:** The URL is fetched inside the job at execution time; network failures use the queue retry/backoff (30s/2m/10m then dead-letter, per Phase 2 D-09).
- **D-04:** Uploaded PDF bytes are copied into `raw/` at submit time as the immutable record; sha256 is computed from the copy. Source drift is N/A for uploaded PDFs.

### Text extraction & raw/ conventions
- **D-05:** Web pages: `readability` + `htmd` crates — article-body detection then clean Markdown text. Pure Rust, no external reader API.
- **D-06:** PDF text: `pdftotext` subprocess (poppler) with LOUD failure if the binary is missing (MoriNet mdbook pattern). No pure-Rust fallback.
- **D-07:** `raw/` files named `raw/YYYY-MM-DD-slug.md` with `sha256` + `source_url` + `ingested` frontmatter; provenance markers read `^[raw/2026-10-29-foo.md]`.
- **D-08:** Source drift (same URL, sha256 changed): auto re-ingest + `log.md` drift entry + pages updated via the normal merge path. sha256 match on unchanged source = no-op (PIPE-02). Drift state tracked via the existing `sync_state` table.

### LLM extraction design
- **D-09:** Two LLM passes per source: **pass 1** extracts candidates (entities/concepts, mentions, central flag + justification, alias variants) as typed JSON via `generate_json_typed`; **pass 2** generates or merges page content per threshold-passing page, seeing the existing page content + the new source.
- **D-10:** Centrality is judged by the LLM per candidate in pass 1 (`central: bool` + one-line justification in the typed schema, criteria spelled out in the prompt); the "2+ sources" half of the threshold is counted programmatically from the entity registry.
- **D-11:** Provenance markers `^[raw/...]` are written by the pass-2 LLM per claim; the deterministic write-path validator enforces presence and valid `raw/` targets (PIPE-05/06).
- **D-12:** Contradiction detection happens in the pass-2 merge call: the LLM compares new claims against existing page sections; on conflict it keeps BOTH claims and sets `contested: true` in frontmatter — never auto-resolves.
- **D-13:** The Phase 3 HTTP endpoint is base-path-aware from day one: nested under the configurable base path (PROJECT.md `/saussunet` decision), no Phase 5 retrofit.

### Thresholds & dedup
- **D-14:** Page-creation thresholds live in `config.toml` `[pipeline]` section (`min_sources = 2` default; sane defaults when absent). Centrality criteria are fixed in the pass-1 prompt. SCHEMA.md remains the format authority, not runtime config.
- **D-15:** Resolve-before-create is deterministic-first: normalize (case, whitespace, punctuation, simple plurals) → exact match against canonical name + `aliases_json`. Fuzzy/LLM-assisted near-miss resolution is deferred.
- **D-16:** Pass-1 LLM proposes alias variants found in the source (acronyms, transliterations, hyphenations); the registry merges them into `aliases_json` — resolve-before-create improves as sources accumulate.
- **D-17:** "One source" for the 2+ threshold = a distinct `raw/` record; drift re-ingest of the same URL replaces the record and does NOT double-count. The registry tracks the source set per entity (schema extension by planner — new column or join table in a new migration).

### Claude's Discretion
- Write-path validator hook point: extend `vault::writer::write_page` vs a pre-write validation module — planner decides; must remain deterministic and reject before any disk write (PIPE-06). Link whitelist derived from `index.md`; frontmatter schema check; provenance presence check.
- Exact job `kind` strings and payload JSON schema (e.g. `ingest_url` / `ingest_pdf`), how the worker pool dispatches by kind (Phase 2 left `kind` generic).
- Pass-1/pass-2 prompt texts and the typed candidate/page schemas (must be tolerant-parse friendly, Phase 2 D-05).
- Module layout (`ingest/`, `extract/`, `pipeline/`): planner decides following ARCHITECTURE.md patterns.
- HTTP surface shape in the binary (axum router, port config, base-path nesting mechanics); keep it minimal — one POST + health check is enough.
- How MockProvider scripts are arranged for two-pass tests (script order is deterministic — one script per source covering both passes).

</decisions>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Project planning (repo)
- `.planning/PROJECT.md` — constraints (FS = truth, base-path `/saussunet` decision, llm-wiki format), Key Decisions table
- `.planning/REQUIREMENTS.md` — PIPE-01..06 definitions
- `.planning/ROADMAP.md` §"Phase 3" — the 5 success criteria this phase must make TRUE

### Prior phase context (repo)
- `.planning/phases/01-workspace-foundations-vault-writer/01-CONTEXT.md` — D-01 (SQLite = rebuildable projection), D-05..D-08 (gen_hash protection, human-owned pages), full-schema-up-front D-02
- `.planning/phases/02-llm-provider-durable-job-queue/02-CONTEXT.md` — D-05 tolerant JSON, D-09..D-12 queue semantics, D-13 scripted MockProvider

### Format authority (outside repo)
- `/home/c/.hermes/profiles/devo/skills/research/llm-wiki/SKILL.md` — llm-wiki conventions: raw/ layer immutability, frontmatter fields, wikilinks, page-creation thresholds, provenance markers, log.md rules. Vault output MUST conform.

### Research (repo)
- `.planning/research/ARCHITECTURE.md` — workspace layout, pipeline/worker patterns
- `.planning/research/PITFALLS.md` — Pitfall 4 (feedback loop: only `raw/` is evidence; refuse vault-internal source paths), provenance/threshold pitfalls

### Existing code (repo)
- `crates/saussunet-core/src/vault/writer.rs` — `write_page`: sole vault mutation entry (path guard, frontmatter validation, ≥2 wikilinks, gen_hash three-way merge, atomic write, registry/index/log projection). PIPE-06 validator builds on this.
- `crates/saussunet-core/src/store/entities.rs` — `upsert_entity`/`resolve_entity`/`list_entities` on `entity_registry` (canonical_name, aliases_json, page_path, page_type) — extend for source-set tracking (D-17)
- `crates/saussunet-core/src/store/jobs.rs` + `crates/saussunet-core/src/queue/mod.rs` — `insert_job(kind, payload_json)`, `spawn_workers(store, provider, vault_id, cfg)` — Phase 3 registers real job kinds and handlers here
- `crates/saussunet-core/src/llm/mod.rs` — `LlmProvider::generate_json_typed`, `MockProvider` scripted (exported for Phase 3 tests), `provider_from_config`
- `crates/saussunet-core/src/store/sync_state.rs` — `upsert_sync_state`/`get_sync_state` — source drift tracking (D-08)
- `crates/saussunet-core/src/vault/frontmatter.rs` — `parse`/`validate_page`/`serialize` — extend frontmatter model for `sources`, `confidence`, `contested` fields
- `crates/saussunet/src/main.rs` — boot wiring (`boot_open`, worker spawn, clap subcommands) — HTTP daemon wiring lands here
- `migrations/0002_registries.sql`, `migrations/0003_jobs_sync_state.sql` — existing schema shapes

### Reference implementation (outside repo)
- MoriNet at `/home/c/Projects/Dev/MoriNet` — LOUD subprocess-failure pattern (mdbook), config.toml pattern (reference only, not a dependency)

</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- `vault::writer::write_page` already enforces frontmatter schema, wikilink floor, gen_hash human-edit protection, atomic writes, index/log projection — the PIPE-06 validator adds link-whitelist + provenance checks upstream of (or inside) this funnel; the LLM never bypasses it.
- `MockProvider::new(script)` scripts responses in order — a full pipeline test scripts pass-1 candidates JSON then pass-2 page JSON(s), zero real LLM calls (CI proof extends Phase 2 D-14).
- `store/jobs.rs` claim/retry/dead-letter machinery is complete — Phase 3 only defines kinds + a handler dispatch.
- `entity_registry` (canonical_name PK, aliases_json) + `resolve_entity` exist from Phase 1 — D-15/D-16 extend them; D-17 needs a source-set addition (new migration).
- `sync_state` store exists with upsert/get — natural home for source_url → sha256 (D-08).
- `vault::index::upsert_entry` + `index.md` content = the link whitelist source for the validator.

### Established Patterns
- sqlx 0.8 / SQLite WAL / vault_id on every table; migrations at workspace root, auto-registered by `sqlx::migrate!`.
- `saussunet-core` zero-axum (CI guard): extraction/LLM/queue logic lives in core; axum router lives in the binary crate.
- All vault mutations funnel through the single serialized writer (Pitfall 6); pass-2 writes go through `write_page`, never direct fs writes.
- Non-destructive at every branch: conflicts skip + record (Phase 1 D-06), contradictions flag `contested` (D-12), drift logs (D-08).
- LOUD failure for external binaries (pdftotext) mirrors MoriNet's mdbook handling.

### Integration Points
- Binary boot: config → store → vault → spawn workers (already wired) + NEW: axum server task serving `POST /ingest` under the configurable base path, inserting jobs the existing pool drains.
- Worker dispatch: `spawn_workers` currently provider-generic; Phase 3 adds kind-based handler routing (ingest_url, ingest_pdf).
- Config: new `[pipeline]` section (thresholds) + HTTP listen/base-path keys in `config.rs` `FileConfig` pattern.

</code_context>

<specifics>
## Specific Ideas

- User deliberately chose HTTP-first entry (`POST /ingest`) over the recommended CLI — the daemon is the primary surface from Phase 3 on; keep the endpoint minimal so Phase 5 UI extends the same server rather than replacing a CLI.
- The two-pass LLM design exists so pass 2 sees BOTH the existing page and the new source — required for merge-with-provenance and contested detection; downstream must not collapse it into a single call.
- Deterministic-only dedup (D-15) is a v1 stance: predictable beats clever; the alias-accumulation loop (D-16) is the mitigation. If near-duplicates still slip through, the fix is more aliases or a later LLM-resolver — not fuzzy matching now.
- "Central to one source" is trusted to the LLM's judgment against prompt criteria (D-10) — the prompt criteria are part of the deliverable, not an afterthought.

</specifics>

<deferred>
## Deferred Ideas

- CLI `saussunet ingest` submitter — superseded by HTTP entry; revisit only if headless-without-daemon use appears (Phase 5 UI is the real surface).
- Fuzzy / LLM-assisted entity resolution for near-misses ("Kubernetes" vs "k8s") — after alias accumulation proves insufficient.
- Surfacing ingestion jobs, dead-letters, and contested pages in the web UI — Phase 5 (REVI-01/02).
- Health lint (orphans, broken links, drift sweep across ALL sources, not just re-submitted ones) — Phase 5 (LINT-01/02).
- Source watching/polling (WebDAV/Drive) that feeds this pipeline automatically — Phase 4 (SRCC-01/02).
- Confidence scoring beyond the boolean contested flag — PIPE-05 says "optional confidence"; v1 ships `contested` only unless planner finds it free.

</deferred>

---

*Phase: 3-Extraction Pipeline*
*Context gathered: 2026-10-29*
