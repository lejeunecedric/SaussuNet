# Phase 2: LLM Provider + Durable Job Queue - Context

**Gathered:** 2026-10-25
**Status:** Ready for planning

<domain>
## Phase Boundary

An abstract `LlmProvider` trait (mock + OpenAI-compatible HTTP implementation) and a durable, crash-safe SQLite job queue with a bounded worker pool. Jobs flow: insert → atomic claim → worker executes against provider → done/retry/dead. A deterministic mock drives the entire pipeline in tests with zero real LLM calls. No extraction pipeline logic (Phase 3), no source watching (Phase 4), no UI (Phase 5).

Requirements: LLMP-01, LLMP-02, SRCC-03.

</domain>

<decisions>
## Implementation Decisions

### LLM endpoint configuration
- **D-01:** Validation order: cloud OpenAI-compatible API first (real endpoint integration test), local Ollama/llama.cpp validated after — same code path, no local-specific logic.
- **D-02:** Single active provider: one `[llm]` section in config.toml (`provider = "openai-compatible" | "mock"`, `base_url`, `model`, `api_key`, `max_concurrency`, `timeout`). No multi-provider registry in v1.
- **D-03:** `api_key` lives in gitignored config.toml (MoriNet pattern) with env override `SAUSSUNET_LLM__API_KEY`. The key must never appear in logs or error messages.
- **D-04:** Default `max_concurrency = 1` (safe for local iGPU LLM), configurable upward for cloud. The worker pool is sized from this value — never unbounded fan-out (LLMP-02).

### Structured JSON output
- **D-05:** Tolerant hybrid strategy: request `response_format=json_object` when the provider supports it, but ALWAYS parse tolerantly (extract the `{...}` block from noisy output). This makes local models that ignore the flag work identically to cloud.
- **D-06:** On unparseable output: 1 in-job repair prompt ("your output is not this JSON, fix it") before marking the job failed. The repair attempt does not consume a queue retry.
- **D-07:** Trait surface: `generate_json(prompt, schema)` at the `LlmProvider` trait level — the provider implementation encapsulates response_format + tolerant parse + repair. Workers only see typed output, never raw strings.
- **D-08:** Non-streaming. Jobs are batches; a full response is sufficient.

### Queue semantics
- **D-09:** Retry policy: 3 retries with exponential backoff (30s / 2min / 10min), then dead-letter. The dead job retains its `error` field.
- **D-10:** Dead-letter = `status='dead'` in the existing `jobs` table (no separate table — schema was frozen in Phase 1 per D-02 of Phase 1). Queryable via `list_jobs(status="dead")`; Phase 5 UI will surface it.
- **D-11:** Claimed-at expiry: 5 minutes. A worker refreshes `claimed_at` every 60s (heartbeat) while the LLM call is in flight. A worker killed with `kill -9` stops heartbeating; its job returns to `pending` after expiry and is re-claimed by another worker — no loss, no duplication (SC3).
- **D-12:** Atomic claim in a single SQLite transaction: `UPDATE jobs SET status='claimed', claimed_at=now WHERE id IN (SELECT id FROM jobs WHERE status='pending' AND vault_id=? ORDER BY priority DESC, created_at LIMIT n)`. Tested with N concurrent workers.

### Deterministic mock & CI
- **D-13:** Mock style: scripted sequence — the mock is fed a programmed queue of responses in code (including failures: invalid JSON, HTTP 5xx, artificial delay/hang). Tests are readable and local.
- **D-14:** CI proof (SC2): trait injection — tests always construct `MockProvider` directly (no config file read), and CI has no API keys. Calling a real LLM is structurally impossible in tests.
- **D-15:** The mock is scriptable for failure paths: invalid JSON (repair test), 5xx (retry/backoff test), hang > expiry (heartbeat/reclaim test). Same entry points as success paths.

### Claude's Discretion
- Exact HTTP client crate (reqwest vs other) and retry/backoff helper (backon vs hand-rolled): planner decides.
- Worker pool implementation shape (tokio tasks + semaphore vs dedicated loop): planner decides, must respect D-04 bound.
- Internal module layout (`llm/`, `queue/`, `worker/`): planner decides following ARCHITECTURE.md patterns.

</decisions>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Project planning (repo)
- `.planning/PROJECT.md` — constraints (abstract LLM provider, no cloud vendor lock-in), workspace split
- `.planning/REQUIREMENTS.md` — LLMP-01, LLMP-02, SRCC-03 definitions
- `.planning/ROADMAP.md` §"Phase 2" — the 4 success criteria this phase must make TRUE

### Prior phase context (repo)
- `.planning/phases/01-workspace-foundations-vault-writer/01-CONTEXT.md` — D-01 (SQLite = rebuildable projection), D-02 (full schema created in Phase 1), sqlx 0.8 pinned

### Research (repo)
- `.planning/research/ARCHITECTURE.md` — workspace layout, worker/queue patterns
- `.planning/research/SUMMARY.md` — gfx1151 structured-output research flag (motivates D-05 tolerant hybrid)
- `.planning/research/PITFALLS.md` — relevant queue/crash-safety pitfalls

### Existing code (repo)
- `migrations/0003_jobs_sync_state.sql` — jobs table schema (status/priority/attempts/claimed_at/error) + `idx_jobs_claim` index
- `crates/saussunet-core/src/store/jobs.rs` — `insert_job`/`list_jobs` exist; claim/retry/expiry is this phase's work
- `crates/saussunet-core/src/config.rs` — no `[llm]` section yet; this phase adds it
- `crates/saussunet-core/Cargo.toml` — reqwest/backon NOT yet deps; this phase adds them

### Reference implementation (outside repo)
- MoriNet at `/home/c/Projects/Dev/MoriNet` — config.toml + env override pattern, gitignored secrets pattern (reference only, not a dependency)

</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- `store/jobs.rs`: `insert_job(vault_id, kind, payload_json)` and `list_jobs(vault_id, status)` — extend with `claim_jobs`, `complete_job`, `fail_job`, `reap_expired_claims`
- `migrations/0003_jobs_sync_state.sql`: jobs table already has all needed columns (status, priority, attempts, claimed_at, error, finished_at) and the claim-order index — no new migration needed for the queue itself
- `config.rs` pattern: `FileConfig` with sections + `load_config_or_default` — add `[llm]` section following the same shape
- Error type in `error.rs` (thiserror) — extend with LLM/queue variants

### Established Patterns
- sqlx 0.8, SQLite WAL, vault_id on every table (Phase 1 D-02/D-03)
- `saussunet-core` has zero axum/HTTP-server dependency — the LLM HTTP client (reqwest) is fine in core (it's an outbound client, not a server dep), but keep the trait boundary clean
- Store methods are `&self` async on a shared `Store` struct — queue claim logic belongs in `store/jobs.rs` as SQL transactions
- Tests use temp vault fixtures (Phase 1 test harness)

### Integration Points
- Binary `main.rs`: after config load, construct the provider from `[llm]` section (mock or openai-compatible), spawn the bounded worker pool wired to the Store
- `LlmProvider` trait lives in core; `MockProvider` exported for downstream test use (Phase 3 will consume it heavily)
- Job `kind` + `payload_json` are generic in Phase 2 — Phase 3 defines the actual extraction job kinds

</code_context>

<specifics>
## Specific Ideas

- User explicitly chose cloud-first validation: get a real OpenAI-compatible cloud call working end-to-end first, then prove the same path against local Ollama — the local model's poor structured-output support is exactly why the tolerant parse (D-05) is mandatory, not optional.
- The heartbeat (D-11) was chosen over a shorter timeout cap specifically to allow very long local LLM calls without risking double-execution — downstream must not "simplify" it away by capping timeouts instead.
- Non-destructive philosophy carries over: dead-lettered jobs keep their error and payload for post-mortem; nothing is silently dropped.

</specifics>

<deferred>
## Deferred Ideas

- Extraction job kinds and payload schemas (what a job actually does) — Phase 3 (PIPE-*)
- Surfacing dead-lettered jobs and queue state in the web UI — Phase 5 (REVI-02)
- Job cancellation / priority re-ordering UI — Phase 5+
- Multi-provider failover (cloud → local fallback) — v2 at best; v1 is single-provider (D-02)
- Streaming responses for real-time UI feedback — no v1 need (D-08)

</deferred>

---

*Phase: 2-LLM Provider + Durable Job Queue*
*Context gathered: 2026-10-25*
