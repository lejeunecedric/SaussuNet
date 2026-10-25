# Architecture Research

**Domain:** Self-hosted Rust knowledge-pipeline server — source watching → LLM pipeline → llm-wiki Markdown vault (Obsidian-compatible), with web UI, bidirectional MCP, and mdbook preview
**Researched:** 2026-10-25
**Confidence:** HIGH (structure, patterns, lib/bin split) — MEDIUM (specific crate choices, to confirm via spike)

## Standard Architecture

The system fits an established pattern: **a continuous source-event-driven ETL pipeline**, with a durable job queue, bounded LLM workers, and the filesystem as the source of truth (the vault), while SQLite only holds sync state and the job queue. This is the same philosophy as MoriNet (axum + sqlx/SQLite + vault FS + mdbook as a subprocess), extended to the "fabrication" side.

### System Overview

```
┌────────────────────────────────────────────────────────────────────────┐
│                  Exposure layer (web binary)                           │
│  ┌────────────┐  ┌────────────────┐  ┌──────────────┐  ┌────────────┐  │
│  │  Web UI    │  │ REST/HTMX API  │  │ Preview      │  │ MCP server │  │
│  │ (config,   │  │ (config, jobs, │  │ mdbook CLI   │  │ (rmcp) —   │  │
│  │  review)   │  │  note review)  │  │ subprocess   │  │ vault→MCP  │  │
│  └─────┬──────┘  └───────┬────────┘  └──────┬───────┘  └─────┬──────┘  │
├────────┴─────────────────┴──────────────────┴────────────────┴─────────┤
│                  Orchestration layer (core)                            │
│  ┌──────────────────────────────────────────────────────────────┐      │
│  │  Watch scheduler (poll Drive/WebDAV/web, MCP streams)       │      │
│  └──────────────┬───────────────────────────────────────────────┘      │
│  ┌──────────────┴──────────────┐   ┌────────────────────────────┐      │
│  │  Durable job queue          │←→ │  Pipeline worker pool      │      │
│  │  (SQLite, states + retries) │   │  (bounded = LLM capacity)  │      │
│  └──────────────┬──────────────┘   └──────────┬─────────────────┘      │
├─────────────────┼─────────────────────────────┼────────────────────────┤
│                 ▼        Domain layer         ▼        (core)          │
│  ┌───────────────────┐ ┌─────────────┐ ┌────────────┐ ┌─────────────┐  │
│  │ Source connectors │ │ Extraction  │ │ Ontology   │ │ Vault       │  │
│  │ drive│webdav│web  │ │ + linking   │ │ layer      │ │ writer +    │  │
│  │ │pdf│mcp-client   │ │ (LLM)       │ │ YAML/OWL/  │ │ lint        │  │
│  │ (MCP pointers)    │ │             │ │ RDF        │ │ (SCHEMA/    │  │
│  └─────────┬─────────┘ └─────────────┘ │ (optional) │ │ index/log) │  │
│            │                           └────────────┘ └──────┬──────┘  │
├────────────┼─────────────────────────────────────────────────┼─────────┤
│            ▼            Persistence layer                   ▼         │
│  ┌──────────────────┐  ┌──────────────────┐  ┌───────────────────────┐ │
│  │ Vault FS         │  │ SQLite           │  │ LLM provider (trait)  │ │
│  │ raw/ │entities/… │  │ sync_state,      │  │ local (llama.cpp/     │ │
│  │ SCHEMA/index/log │  │ jobs, history    │  │ ROCm) or cloud API    │ │
│  │ = source of      │  │ = derived state, │  │ local or remote       │ │
│  │   truth          │  │   rebuildable    │  └───────────────────────┘ │
│  └──────────────────┘  └──────────────────┘                            │
└────────────────────────────────────────────────────────────────────────┘
```

### Component Responsibilities

| Component | Responsibility | Typical Implementation |
|-----------|----------------|------------------------|
| **Source connectors** | Detect changes in a source (Drive, WebDAV, web/PDF, inbound MCP), normalize into "source events" (new/changed/deleted). MCP connectors produce **pointer notes only**, never a copy of the remote content. | `SourceConnector` trait (async_trait) + one module per source: `google-drive3` (changes.list API + pageToken), WebDAV via `reqwest` PROPFIND/ETag (or `dav-client`), web via fetch + readability extraction, MCP via `rmcp` client |
| **Watch scheduler** | Schedule per-source polls (intervals, jitter, error backoff), run persistent MCP streams | One `tokio::spawn` loop per source + `tokio-cron-scheduler` or homegrown intervals; a `CancellationToken` per source for pause/stop from the UI |
| **Ingestion queue (job queue)** | Durable work queue for extraction: states `pending → running → done/failed(retry)`, sha256 dedup, priority, crash recovery | SQLite `jobs` table + worker loop claiming via immediate transaction + `UPDATE … RETURNING`; bounded `tokio::sync::mpsc` channels only for "work available" signaling |
| **Extraction pipeline (workers)** | Consume jobs: read `raw/`, call the LLM (summary, entities, concepts, wikilinks, frontmatter), produce structured **page proposals** | Bounded `tokio` task pool (= LLM capacity); LLM output schema-validated (JSON + `schemars`/`serde_json`) before writing |
| **Abstract LLM provider** | Single chat/completion interface with structured output, whichever local or API backend | `LlmProvider` trait (async_trait); impls: OpenAI-compatible (llama.cpp, vLLM, ROCm) + cloud APIs; internal semaphore for max concurrency |
| **Ontology layer** | Load the optional ontology (embedded default YAML, imported OWL/RDF), expose classes/relations/display types to the pipeline and page validation | `Ontology` trait; YAML = simple internal model; RDF via `sophia` (0.10) or the `oxrdf`/`oxrdfio` family; OWL via `horned-owl` if real OWL import is needed (evaluate in spike — likely import → projection onto the internal model, no full reasoner) |
| **Vault writer** | Sole author of the vault: writes `raw/` (immutable, sha256 frontmatter), creates/merges `entities/`/`concepts/` pages, updates `index.md`/`log.md`, enforces SCHEMA.md (creation thresholds, tag taxonomy) | `vault::writer` module with a single serialized writer task; atomic write-temp-rename; optional git for history |
| **Lint** | Vault health: orphans, broken links, frontmatter, index completeness, raw sha256 drift, flagged contradictions, log rotation | Periodic scan + after each ingestion batch; report persisted (`lint_reports`) + surfaced in the UI; reuses the llm-wiki pattern's rules |
| **MCP server (outbound)** | Expose the vault over MCP: search, page reads, log/index reads — read-only | `rmcp` (same SDK as MoriNet Phase 10, `server` feature + streamable HTTP transport); mounted on the binary's axum router, read logic lives in the lib |
| **MCP client (inbound)** | Consume external MCP servers: list resources, store a **pointer note** (access-point URI, metadata), read on demand as a stream | `rmcp` client; the pointer is a normal note with `type: pointer` + `mcp_uri:` frontmatter; remote content is never cached into the vault |
| **Web UI / API** | Source + ontology config, pipeline tracking (queue, errors, logs), review/edit of generated notes, manual poll/lint triggers | axum 0.8 + handlers; simple rendering (templates or a light SPA); editing writes directly into the vault via the vault writer (under the lock) |
| **Mdbook preview** | Compile the vault with mdbook on demand/periodically and serve the generated `book/` | `std::process::Command::new("mdbook")` like MoriNet (`sources.rs`); **LOUD failure if mdbook is missing from PATH**; build triggered by ingestion-batch completion, not per file |
| **Store (SQLite)** | Per-source sync state (Drive pageToken, WebDAV ETags), job queue, history/events, lint reports, human review statuses | `sqlx` 0.8 sqlite runtime-tokio + migrations in `migrations/` (MoriNet pattern) |

## Recommended Project Structure

Two-member Cargo workspace (project constraint):

```
saussunet/
├── Cargo.toml                      # [workspace] members = crates/*
├── migrations/                     # sqlx SQL migrations (state, jobs, lint, review)
├── crates/
│   ├── saussunet-core/             # THE lib — zero axum, zero HTTP server
│   │   └── src/
│   │       ├── lib.rs              # Public surface: types + services, no transport
│   │       ├── config.rs           # Config types (TOML): sources, llm, ontology, vault
│   │       ├── store/              # sqlx: sync state, jobs, history, lint_reports
│   │       │   ├── mod.rs
│   │       │   ├── jobs.rs         # Durable queue: claim, retry, dead-letter
│   │       │   └── sync_state.rs   # pageTokens, ETags, last_seen per source
│   │       ├── scheduler.rs        # Per-source poll loops, backoff, CancellationToken
│   │       ├── connectors/         # One submodule per source type
│   │       │   ├── mod.rs          # SourceConnector trait + SourceEvent
│   │       │   ├── drive.rs        # google-drive3, changes.list + pageToken
│   │       │   ├── webdav.rs       # PROPFIND + ETag/sha256
│   │       │   ├── web.rs          # article fetch + PDF (pdf-extract / lol_pdf)
│   │       │   └── mcp_in.rs       # rmcp client → pointer notes only
│   │       ├── vault/              # All vault reads/writes (source of truth)
│   │       │   ├── mod.rs          # Vault struct: single write lock
│   │       │   ├── schema.rs       # SCHEMA.md: parsing, tag taxonomy, thresholds
│   │       │   ├── writer.rs       # immutable raw/, pages, frontmatter, wikilinks
│   │       │   ├── index.rs        # index.md (sections, counters)
│   │       │   ├── log.rs          # log.md append + rotation
│   │       │   └── lint.rs         # llm-wiki health rules
│   │       ├── extract/            # LLM pipeline: prompts, output parsing, merge
│   │       │   ├── mod.rs          # Worker loop: job → LLM → PageProposal → writer
│   │       │   └── prompts.rs
│   │       ├── llm/                # Provider abstraction
│   │       │   ├── mod.rs          # LlmProvider trait + concurrency semaphore
│   │       │   ├── openai_compat.rs# llama.cpp/vLLM/OpenAI (configurable base_url)
│   │       │   └── mock.rs         # Deterministic provider for tests
│   │       ├── ontology/           # Ontology layer
│   │       │   ├── mod.rs          # Ontology trait + internal model (classes/relations)
│   │       │   ├── default_yaml.rs # Embedded default llm-wiki ontology
│   │       │   └── rdf.rs          # OWL/RDF import (sophia / horned-owl) → internal model
│   │       ├── mcp_server.rs       # rmcp Resource/Tool impls over the vault (no transport)
│   │       └── events.rs           # Internal bus (tokio broadcast): JobDone, LintDone…
│   └── saussunet/                  # Standalone web binary
│       └── src/
│           ├── main.rs             # clap, config, axum + scheduler + workers wiring
│           ├── routes/             # axum handlers: config, sources, jobs, review, lint
│           ├── ui/                 # Templates / static assets
│           ├── preview.rs          # mdbook build subprocess + serving book/
│           └── mcp_mount.rs        # Mounting core MCP onto the axum router
├── assets/                         # Default YAML ontology, SCHEMA.md templates
└── tests/                          # Integration tests (temp vault + mock llm)
```

### Structure Rationale

- **`crates/saussunet-core` with no HTTP-server dependency whatsoever**: this is the boundary that guarantees MoriNet (or any host) can depend on the lib without swallowing axum. The core exposes **services** (async functions + traits); the binary chooses transports (axum, rmcp transport, CLI). MoriNet is the proof of need: it already has its own MCP and its own web server.
- **`connectors/` as submodules behind a trait**: each source has its own auth, pagination, and change-detection logic; `SourceConnector` is the only contract the scheduler sees. Adding a source = one file, no refactor.
- **`vault/` centralizes ALL writes into the vault**: a single authoring module = the llm-wiki invariants (immutable raw, index/log always current, creation thresholds) enforced in one place, testable against a temp vault.
- **`migrations/` at the workspace root** (like MoriNet): sqlx migrations live outside the crates so lib and bin share the same schema.
- **`llm/mock.rs`**: a deterministic provider is essential to test the full pipeline (extraction → proposals → writing) without a GPU or API.

## Architectural Patterns

### Pattern 1: Durable job queue in SQLite (not in memory)

**What:** every source change becomes a `jobs` row (`id, source_id, raw_path, kind, status, attempts, last_error, priority, created_at, claimed_at`). Workers claim atomically, process, mark `done/failed`. Bounded `tokio::mpsc` channels only signal "work available", never store the work.
**When to use:** whenever processing is long (LLM: seconds to minutes), can fail, and must survive restarts — exactly this case.
**Trade-offs:** a bit of SQL plumbing vs entirely avoiding an external queue like Redis; SQLite serializes writes but at this throughput (a few jobs/s max, LLM is the bottleneck) it's negligible. A crash mid-job is recovered via expired `claimed_at` → re-claim.

```rust
// Atomic SQLite claim (sqlx) — one worker wins, others come back empty-handed
let job: Option<Job> = sqlx::query_as(
    "UPDATE jobs SET status='running', claimed_at=? \
     WHERE id = (SELECT id FROM jobs WHERE status='pending' \
                 ORDER BY priority DESC, created_at LIMIT 1) RETURNING *")
    .fetch_optional(&pool).await?;
```

### Pattern 2: Boundary traits for everything interchangeable

**What:** three traits structure the system — `SourceConnector` (poll → `Vec<SourceEvent>`), `LlmProvider` (chat + structured output), `Ontology` (classes/relations/display types). Implementations are chosen at configuration time and injected into the core services.
**When to use:** every variability axis the project declares (local/API, multiple sources, optional ontology) deserves its trait; it's also what makes the core integrable by MoriNet.
**Trade-offs:** slight indirection (async_trait / boxing); deliberately avoided: hot dynamic plugins (`.so`) — overkill for a self-hosted server, recompilation suffices.

```rust
#[async_trait]
pub trait SourceConnector: Send + Sync {
    /// Poll once. Returns changes since `state` (pageToken/ETag).
    /// The scheduler persists `state` only if all events were enqueued.
    async fn poll(&self, state: &SyncState) -> Result<PollOutcome>;
}
pub struct PollOutcome { pub events: Vec<SourceEvent>, pub new_state: SyncState }
```

### Pattern 3: Vault FS = source of truth, SQLite = rebuildable projection

**What:** all content (raw, pages, index, log, SCHEMA) lives in the Markdown vault; SQLite only holds sync state, the queue, history, and reports. A `saussunet reindex` rebuilds the database by scanning the vault (frontmatter + sha256). Conversely, losing SQLite costs only sync tokens and history — never content.
**When to use:** mandatory here: the vault must remain usable/editable standalone in Obsidian, and MoriNet must be able to serve it without SaussuNet.
**Trade-offs:** lint and the UI sometimes scan the FS (cost linear in page count — acceptable at personal-vault scale, a few thousand files); avoids the classic diverging dual-truth trap of tools that own their own DB alongside Obsidian.

## Data Flow

### Main flow: source → vault

```
[Scheduler polls Drive/WebDAV/web]
    ↓ SourceEvent {url/path, mtime, etag}
[Dedup] — content sha256 == existing raw? → skip (else flag drift)
    ↓ new/changed
[Vault writer] → raw/articles/foo.md (+frontmatter source_url, ingested, sha256)
    ↓
[Store] INSERT job {kind: ingest, raw_path} (status=pending)
    ↓ bounded mpsc signal (backpressure)
[LLM worker] claim job → reads raw + index.md + ontology → prompt → JSON
    ↓ PageProposal[] {action: create|update, path, frontmatter, sections, wikilinks}
[Validation] schema + creation thresholds + tags ∈ taxonomy → reject otherwise
    ↓
[Vault writer (single lock)]
    ├─ creates/merges entities/*.md, concepts/*.md (bump `updated`, mark contradictions)
    ├─ updates index.md (section, alpha order, counters)
    └─ appends log.md: `## [date] ingest | Source Title` + files touched
    ↓
[Store] job=done · [events] JobBatchDone
    ↓ (debounced)
[Lint] health scan → lint_reports · [Preview] mdbook build (if enabled)
```

### Inbound MCP flow (pointers, never copies)

```
[External MCP stream] → list_resources / notifications
    ↓
[Vault writer] pointer note: entities/x.md
    frontmatter: type: pointer, mcp_uri: "mcp://server/resource/x", title, observed_on
    body: light contextual summary (generated once) + the access-point address
    ↓ on demand (UI or external agent)
[rmcp client] reads remote content as a stream → displayed, never written into raw/
```

### Human review flow (quality guardrail)

```
[Review UI] lists recent jobs + pages created/modified (from store + vault)
    ↓ edit (textarea → PUT)
[Vault writer] writes into the vault (lock) + bump `updated` + log.md `## [date] update | …`
    ↓ `reviewed: true` flag in frontmatter (or store table) → the pipeline won't
      blindly rewrite a reviewed section without marking the divergence
```

### Outbound MCP exposure flow

```
[External agent] tools/call vault_search {query} → [core mcp_server] index+FS scan
[External agent] tools/call read_page {path}     → vault read (read-only)
[External agent] tools/call recent_log {n}       → tail of log.md
```

### Key Data Flows

1. **Continuous ingestion:** poll → sha256 dedup → `raw/` → job → LLM → pages + index + log → lint → preview. Everything is idempotent: replaying an event duplicates nothing (sha256 + thresholds + page existence).
2. **State watching:** each poll persists its token (Drive pageToken, WebDAV ETag) **after** successful enqueue of the events — never before (worst case a re-poll, never a loss).
3. **Backpressure:** the SQLite queue is unbounded in depth but consumption is bounded by the LLM pool; signaling channels are bounded (a lost signal is caught by the next poll; the SQL queue is authoritative).
4. **Rebuild:** `reindex` scans the vault → repopulates the page-registry table; `lint` compares vault ↔ store ↔ index.

## Concurrency and Backpressure

- **One tokio task per watched source** (poll interval + jitter + exponential backoff on error). MCP streams run as long-lived tasks with reconnection.
- **Bounded extraction worker pool** (`tokio::sync::Semaphore`), sized to real LLM capacity: on local Strix Halo, 1–2 slots (the GPU serializes); on cloud APIs, 4–8 depending on quotas. Configurable, conservative default.
- **Single vault writer**: page proposals are merged by one serialized writer task (mpsc channel of `PageProposal`). Two workers never touch `index.md` in parallel. Batch merging: group a batch's proposals before writing (llm-wiki "bulk ingest" pattern: one search pass over existing pages, one creation pass, a single index/log update).
- **Atomic writes**: write temp + rename; the vault stays consistent even if the process dies mid-write (Obsidian watches the FS live).
- **Cancellation/pause:** a `CancellationToken` per source and per long job; the UI can kill a runaway LLM job (re-claim after claim expiry).
- **mdbook:** one build at a time (queue of 1, debounced); build triggered by batch end, not per file.

## Persistence

| Store | Contents | Role |
|----------|---------|------|
| **Vault FS** (configured `vault/`) | `SCHEMA.md`, `index.md`, `log.md`, `raw/`, `entities/`, `concepts/`, MCP pointers | **Source of truth** for content; editable in Obsidian; optional git for history |
| **SQLite** (`data/saussunet.db`, sqlx migrations) | `sources` (dynamic config), `sync_state` (pageToken/ETag per source), `jobs` (queue + history: status, attempts, errors, durations), `lint_reports`, `reviews` (review statuses), `events` (activity journal for the UI) | Sync state + durable queue + history; content-derived parts **rebuildable** via reindex |
| **`config.toml`** | vault/data paths, LLM providers (base_url, model, concurrency), source definitions (or in SQLite if edited via UI), poll intervals, mdbook options | Static startup config, MoriNet pattern; sources modified via the UI live in SQLite with TOML override |

Suggested key tables: `jobs(id, source_id, kind, payload_json, status, priority, attempts, error, created_at, claimed_at, finished_at)` with an index on `(status, priority, created_at)`; `sync_state(source_id PK, token, etag, last_poll_at, last_error)`; `page_registry(path PK, title, type, tags_json, updated, reviewed)` — a pure projection of the vault, rebuildable.

## Scaling Considerations

| Scale | Architecture Adjustments |
|-------|--------------------------|
| Personal use (0–1 active sources, <5k pages) | Single tokio monolith, everything in-process: this is the v1 target, nothing more |
| 5–50k pages, 5–20 sources | Bottleneck = LLM (already bounded); lint and index.md become the FS costs — make lint incremental (pages touched by the batch) and enable the llm-wiki index split rule (>200 entries → topic-map) |
| Multi-vault / shared host (MoriNet) | One `VaultService` instance per vault in the core; SQLite per vault or a `vault_id` column; no architecture change |

### Scaling Priorities

1. **First bottleneck: long LLM jobs.** One large badly-chunked PDF can hog a slot for minutes. Mitigations: chunk the raw into parent/child jobs, per-job timeout, separate pool for "large documents" vs "quick notes".
2. **Second bottleneck: `index.md` and lint on large vaults.** Single writer + full index rewrite per batch = contention and IO. Mitigations: batch index updates (end of batch), in-memory parse cache (invalidated by mtime).
3. **Third: SQLite write lock** under poll spam — irrelevant at the targeted throughput; WAL mode suffices.

## Anti-Patterns

### Anti-Pattern 1: Making `saussunet-core` depend on axum (or the MCP transport)

**What people do:** put HTTP handlers and rmcp transport wiring in the lib "because it's simpler".
**Why it's wrong:** MoriNet can no longer depend on the lib without pulling in a second web server and a second transport; the reusability boundary (a hard project constraint) dies at the first `use axum::` in the core.
**Do this instead:** the core exposes services and data structs; the binary (and later MoriNet) owns the transport. CI guard: a dependency test (or simply review) asserting axum is absent from `saussunet-core/Cargo.toml`.

### Anti-Pattern 2: Treating SQLite as the source of truth for notes

**What people do:** store note content in the database and "export" to the vault.
**Why it's wrong:** Obsidian edits the FS live; the dual truth diverges (lost edits, conflicts). The project requires a standard standalone vault that MoriNet must be able to serve without SaussuNet.
**Do this instead:** vault FS = truth; SQLite = rebuildable derived state (`reindex`). Any info the UI must display is readable from the vault (frontmatter, log).

### Anti-Pattern 3: Copying inbound MCP source content into `raw/`

**What people do:** "normalize" MCP resources by downloading them into raw/ like other sources.
**Why it's wrong:** violates a project constraint (MCPs are streams, not archives); the vault bloats with stale, untraceable copies and the remote source loses its truth status.
**Do this instead:** pointer note (address + metadata + light summary), on-demand reads via the MCP client; lint does not sha256-check pointers.

### Anti-Pattern 4: Unbounded LLM concurrency / parallel page writes

**What people do:** `tokio::spawn` per job with no semaphore, each worker writing its own pages and index.md.
**Why it's wrong:** on a local GPU, N simultaneous jobs = memory thrash and exploding latencies; two index.md writers = corrupted file or lost entries (the llm-wiki pattern forbids inconsistent indexes).
**Do this instead:** bounded pool + single vault writer + batch-merged proposals.

### Anti-Pattern 5: Regenerating the whole vault on every ingestion

**What people do:** rerun a "full build" (all sources → all pages) on every detected change.
**Why it's wrong:** costs O(corpus) LLM calls where llm-wiki is designed to be incremental (creation thresholds, targeted updates); loses human edits; log.md becomes rebuild noise.
**Do this instead:** strictly incremental processing per source event; full rebuild only in explicit recovery mode (reindex/lint repair).

### Anti-Pattern 6: The monolithic `server.rs`

**What people do:** cram everything into one giant handler module (MoriNet bears the trace: `server.rs` ≈ 4,500 lines).
**Why it's wrong:** readability and testability collapse; it's the direct lesson from the sibling project.
**Do this instead:** split `routes/` per resource from day one; logic lives in the core, handlers stay under ~30 lines.

## Integration Points

### External Services

| Service | Integration Pattern | Notes |
|---------|---------------------|-------|
| Google Drive | `google-drive3` (or REST `reqwest` + OAuth); `changes.list` with persisted pageToken, download by fileId; Docs export→md/pdf | pageToken never expires → perfect for `sync_state`; per-user quota, backoff on 429; `drive.readonly` scope |
| WebDAV (Nextcloud, etc.) | `reqwest` PROPFIND + ETag/Last-Modified, GET for content (or `dav-client`) | ETags sometimes weak/missing → fallback to content sha256; handle non-UTF8 and names with spaces |
| Web / PDF | fetch + article extraction (readability-like); PDF via `pdf-extract` (simple) or `lol_pdf` (modern, quality to be evaluated in a spike) | anti-bot: clean UA, timeout, failure = job warning not fatal; lint "source drift" relies on raw sha256 |
| Inbound MCP servers | `rmcp` client (streamable HTTP / stdio); list+read → pointers; `list_changed` notifications if supported | never block the scheduler on a slow MCP server: short timeout, backoff reconnection |
| Local LLM | OpenAI-compatible endpoint (llama.cpp server / vLLM on ROCm gfx1151); configurable `base_url` | structured output (JSON mode / grammar) to verify per backend → fallback tolerant parsing + strict validation |
| Cloud LLM API | Same trait, base_url + key | privacy: vault content goes to the provider → per-source "local-only" flag |
| mdbook | Subprocess `mdbook build` (MoriNet `sources.rs` pattern); serve `book/` via `tower-http::fs` | present in PATH, **LOUD failure if missing** (explicit install message, never a silent fallback); generate an ephemeral SUMMARY.md for preview without polluting the vault |
| MoriNet (downstream) | Cargo dependency on `saussunet-core`; MoriNet serves the produced vault with its own mdbook/MCP | the lib must impose neither axum nor an application schema on the host |

### Internal Boundaries

| Boundary | Communication | Notes |
|----------|---------------|-------|
| Scheduler ↔ Connectors | direct `SourceConnector` trait call | scheduler persists `new_state` only after successful enqueue |
| Scheduler/Connectors ↔ Queue | SQLite `jobs` write + bounded mpsc signal | the SQL queue is always authoritative; the signal is an optimization |
| Queue ↔ Extraction workers | atomic SQLite claim + LLM semaphore | one worker = one job at a time, per-job timeout |
| Workers ↔ Vault writer | mpsc `PageProposal` (single writer) | serialized write lock, atomic writes |
| Workers/UI ↔ LLM | `LlmProvider` trait behind a semaphore | deterministic mock in tests |
| Pipeline ↔ Ontology | `Ontology` trait, read-only during extraction | the ontology constrains allowed tags/types/relations |
| Core ↔ Web binary | service calls + `events` bus (tokio::broadcast) for real-time UI tracking | SSE/WS for pipeline tracking = optional; light polling suffices in v1 |
| Binary ↔ mdbook | subprocess, never the mdbook lib | total decoupling, cf. MoriNet |

## Sources

- llm-wiki pattern (Karpathy): gist 442a6bf555914893e9891c11519de94f — 3 layers, frontmatter/wikilinks/index/log conventions, creation thresholds, lint (sha256 drift, orphans, contradictions); local skill `llm-wiki` v2.1.0 (ingestion/lint/bulk-ingest rules reused as-is)
- MoriNet (`/home/c/Projects/Dev/MoriNet`): reference stack axum 0.8 + sqlx 0.8 sqlite + rmcp 3.7 (server, streamable-http) + git2; mdbook-subprocess pattern with LOUD failure (`src/sources.rs`, `src/server.rs:731`); lib+bin single crate; anti-pattern `server.rs` 4,562 lines
- Google Drive API `changes.list` (developers.google.com): persistent pageToken, pagination, scopes
- Crates verified on docs.rs (2026-10): `google-drive3` 7.0 (full drive v3), `sophia` 0.10 (RDF/Linked Data), `horned-owl` 1.0 (OWL 2, RDF/XML, Manchester), `oxrdf`/`oxrdfio`/`rudof_rdf` family (Oxigraph alternative), `rmcp` 3.7 (via MoriNet)
- Choices to confirm via spike: `pdf-extract` vs `lol_pdf`; `dav-client` vs manual reqwest PROPFIND; `tokio-cron-scheduler` vs homegrown tokio intervals; real OWL import vs internal YAML projection

---
*Architecture research for: Rust server continuously fabricating llm-wiki knowledge vaults (ETL + LLM + MCP + web pipeline)*
*Researched: 2026-10-25*

## Suggested Build Order (component dependencies)

Each step is deliverable and testable on its own; arrows = "requires".

1. **Workspace foundations** — Cargo workspace, `saussunet-core` + `saussunet` (skeleton bin), `config.rs`, SQLite migrations (`sources`, `sync_state`, `jobs`), store. *(root of everything)*
2. **Vault writer + llm-wiki format** — vault init (SCHEMA/index/log), immutable raw + sha256, page writing + frontmatter + wikilinks, index/log, writer lock, atomic writes. **No LLM**: we can already write a correct vault by hand via the API. *(← 1)*
3. **Abstract LLM provider + mock** — `LlmProvider` trait, OpenAI-compatible impl, deterministic mock, semaphore. *(← 1, independent of 2)*
4. **Extraction pipeline** — prompts, validated JSON output (`PageProposal`), worker loop over the queue, batch merge into the vault writer, creation thresholds. *(← 2, 3)*
5. **Connectors** — in order of simplicity: **web/PDF** (simplest, immediate end-to-end), then **WebDAV** (ETag), then **Drive** (OAuth + changes.list), then **inbound MCP** (pointers). Each one = poll → dedup → raw → job. *(← 1, 4 for end-to-end; a connector can ship before 4 with jobs pending)*
6. **Watch scheduler** — per-source poll loops, intervals/jitter/backoff, token persistence, CancellationToken. *(← 5)*
7. **Ontology layer** — first the embedded default YAML ontology (constrains tags/types), then RDF/OWL import (`sophia`/`horned-owl` → internal model) as an option. *(← 2, 4; deferrable late without breaking the pipeline)*
8. **Lint** — llm-wiki rules (orphans, broken links, frontmatter, index, sha256 drift, log rotation), `lint_reports`, post-batch + manual triggers. *(← 2)*
9. **Web UI/API** — source config routes, pipeline tracking (queue/jobs/errors, via the events bus), note review/edit (vault write + reviewed flag), manual poll/lint triggers. *(← 4, 6, 8)*
10. **Mdbook preview** — debounced build subprocess + static serving, LOUD failure if missing. *(← 2; independent of the rest)*
11. **Outbound MCP server** — read-only rmcp tools (search/read/log) mounted on the axum router. *(← 2; transport in the bin, logic in the core)*
12. **MoriNet reusability hardening** — audit of the core's public surface (zero axum, no global singletons, injected pool/vault path), lib docs, a minimal "MoriNet-like host" integration test. *(← everything)*

v1 critical path (demo "one source → a living vault"): **1 → 2 → 3 → 4 → 5(web) → 6 → 9(minimal)**. Steps 7, 10, 11 are independent vertical extensions.
