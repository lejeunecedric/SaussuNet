# Phase 2: LLM Provider + Durable Job Queue - Discussion Log

> **Audit trail only.** Do not use as input to planning, research, or execution agents.
> Decisions are captured in CONTEXT.md — this log preserves the alternatives considered.

**Date:** 2026-10-25
**Phase:** 02-llm-provider-durable-job-queue
**Areas discussed:** LLM endpoint config, Structured JSON output, Queue semantics, Deterministic mock & CI

---

## LLM endpoint config

| Option | Description | Selected |
|--------|-------------|----------|
| Ollama first | http://localhost:11434/v1, zero GPU config, easiest on Strix Halo | |
| llama.cpp server first | llama-server --port 8080, more control (ctx, GPU layers) | |
| Cloud first | Validate against a cloud API first, local after | ✓ |

| Option | Description | Selected |
|--------|-------------|----------|
| config.toml + env override | api_key in gitignored config.toml (MoriNet pattern) + SAUSSUNET_LLM__API_KEY env override | ✓ |
| Env only | api_key never in a file | |
| Two files | config.toml + secrets.local.toml | |

| Option | Description | Selected |
|--------|-------------|----------|
| 1 worker default | Safe for local iGPU LLM, configurable up for cloud | ✓ |
| 2 workers | Ryzen 395 can hold 2 concurrent requests | |
| 4 workers | Cloud-sized default | |

| Option | Description | Selected |
|--------|-------------|----------|
| Single [llm] section | provider/base_url/model/api_key/max_concurrency/timeout | ✓ |
| Multi named providers | Per-job provider choice — out of v1 scope | |
| You decide | Planner decides shape | |

**User's choice:** Cloud-first validation; config.toml + env override; 1 worker default; single `[llm]` section.
**Notes:** Code must still support local endpoints identically (same OpenAI-compatible path).

---

## Structured JSON output

| Option | Description | Selected |
|--------|-------------|----------|
| Tolerant hybrid | Request response_format when supported, ALWAYS parse tolerantly ({...} extraction) | ✓ |
| Strict first | Require response_format, fail otherwise — breaks on local endpoints ignoring the flag | |
| Tolerant only | Never send response_format | |

| Option | Description | Selected |
|--------|-------------|----------|
| Repair prompt in job | 1 repair prompt before marking job failed | ✓ |
| No repair | Parse failure = job failed, queue retry loop handles it | |
| You decide | Planner weighs cost/robustness | |

| Option | Description | Selected |
|--------|-------------|----------|
| generate_json on trait | Provider encapsulates response_format + parse + repair; workers see typed output | ✓ |
| Raw chat() only | Parse/validation in workers | |
| Both | chat() low-level + generate_json() on top | |

| Option | Description | Selected |
|--------|-------------|----------|
| Non-streaming | Jobs are batches, full response only | ✓ |
| Streaming | Prep for future real-time UI | |

**User's choice:** Tolerant hybrid; in-job repair prompt; `generate_json(prompt, schema)` on the trait; non-streaming.

---

## Queue semantics

| Option | Description | Selected |
|--------|-------------|----------|
| 3 retries, exp backoff (30s/2min/10min), then dead-letter | Bounded, readable, dead job keeps its error | ✓ |
| 5 fixed retries at 1min | Simpler, worse for long API outages | |
| Infinite retries with age cap | Risk of silent loops | |

| Option | Description | Selected |
|--------|-------------|----------|
| status='dead' in jobs table | No separate table, queryable, Phase 5 UI surfaces it | ✓ |
| Separate jobs_dead_letter table | Cleaner archive but Phase 1 froze the schema (D-02) | |
| No dead-letter | Terminal 'failed', manual SQL replay | |

| Option | Description | Selected |
|--------|-------------|----------|
| Expiry 15 min + auto re-claim | kill -9 frees the job after expiry | |
| Expiry 5 min | Fast recovery; double-execution risk if a call exceeds 5 min | ✓ |
| Expiry configurable, no auto re-claim | Human decides | |

| Option | Description | Selected |
|--------|-------------|----------|
| Atomic SQLite claim | Single-transaction UPDATE...WHERE id IN (SELECT...LIMIT n), tested with N concurrent workers | ✓ |
| In-memory queue + periodic persist | Loses crash-safety guarantee | |
| You decide | Planner picks exact mechanism | |

| Option | Description | Selected |
|--------|-------------|----------|
| Timeout LLM < expiry | HTTP timeout bounded under expiry — no double-execution possible | |
| Heartbeat | Worker refreshes claimed_at every 60s during the call; allows very long calls | ✓ |
| Both | Bounded timeout AND safety heartbeat | |

**User's choice:** 3 retries exp backoff → dead; `status='dead'` in jobs table; 5-min expiry (not the recommended 15); atomic single-transaction claim; heartbeat every 60s to reconcile expiry with long calls.
**Notes:** User deliberately chose the shorter expiry + heartbeat combination over the recommended longer expiry — downstream must implement the heartbeat, not "simplify" it into a timeout cap.

---

## Deterministic mock & CI

| Option | Description | Selected |
|--------|-------------|----------|
| Scripted sequence | Mock fed a programmed response queue in code (incl. failures) | ✓ |
| Replayed fixtures | JSON fixtures indexed by prompt hash | |
| Both | Scripted for unit, fixtures for one integration test | |

| Option | Description | Selected |
|--------|-------------|----------|
| Trait injection | Tests always construct MockProvider; CI has no API keys — real LLM structurally impossible | ✓ |
| Runtime guard | Test asserts provider==mock in CI | |
| Egress blocking | CI blocks outbound network | |

| Option | Description | Selected |
|--------|-------------|----------|
| Mock scriptable for failures | Invalid JSON (repair test), 5xx (retry test), hang > expiry (heartbeat/reclaim test) | ✓ |
| Happy path only | Ad-hoc fake providers per error test | |

**User's choice:** Scripted sequence; trait injection as the CI proof; mock scriptable for all failure paths.

---

## Claude's Discretion

- HTTP client crate (reqwest vs other) and backoff helper (backon vs hand-rolled)
- Worker pool implementation shape (tokio tasks + semaphore vs dedicated loop), respecting the D-04 bound
- Internal module layout (llm/, queue/, worker/) following ARCHITECTURE.md

## Deferred Ideas

- Extraction job kinds/payload schemas — Phase 3
- Dead-letter/queue surfacing in UI — Phase 5 (REVI-02)
- Multi-provider failover — v2
- Streaming for real-time UI — no v1 need
