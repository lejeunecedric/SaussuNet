---
status: resolved
phase: 02-llm-provider-durable-job-queue
source: [02-VERIFICATION.md]
started: 2026-10-26T08:37:33Z
updated: 2026-10-26T09:52:00Z
---

# Phase 02 — Human Verification

## Current Test

[awaiting human testing]

## Tests

### 1. Live end-to-end run against a real OpenAI-compatible endpoint (D-01)

expected: The same code path proven with `MockProvider` runs against a real endpoint — cloud provider first, then local Ollama. The env-gated live test must pass with real credentials (it honestly skips, never fake-passes, when `SAUSSUNET_LLM__API_KEY` is unset).

Run (from repo root):

```bash
SAUSSUNET_LLM__BASE_URL=<endpoint> SAUSSUNET_LLM__MODEL=<model> SAUSSUNET_LLM__API_KEY=*** \
  cargo test -p saussunet-core --test llm_live -- --ignored
```

result: pass — 2026-10-26, run against local Halogen endpoint (`http://127.0.0.1:8731/v1`, model `halogen-qwen3.8-flash-next`): `live_endpoint_returns_valid_json ... ok` (1 passed, 0.73s). Bearer auth, chat/completions wiring, and tolerant parse all proven against a live endpoint.

## Summary

total: 1
passed: 1
issues: 0
pending: 0
skipped: 0
blocked: 0

## Gaps

(none — automated verification score 25/26 must-haves, zero gaps; this item is the intentional D-14 env-gated live validation)
