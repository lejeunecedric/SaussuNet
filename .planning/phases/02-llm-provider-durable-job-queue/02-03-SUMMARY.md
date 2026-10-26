---
phase: 02-llm-provider-durable-job-queue
plan: 03
subsystem: llm-provider
tags: [rust, async-trait, dyn-compatibility, serde-json, tolerant-parse, repair-prompt, mock-provider, tokio]

requires:
  - phase: 02-llm-provider-durable-job-queue (plan 01)
    provides: "CoreError::Llm/LlmParse variants, async-trait + serde_json deps, tokio time feature"
  - phase: 01-workspace-foundations-vault-writer
    provides: "module-shape discipline (store/mod.rs, vault/mod.rs Arc-shared + tokio::sync::Mutex precedent), test file shape"
provides:
  - "LlmProvider trait (async_trait, dyn-compatible): complete + default generate_json with D-05 tolerant parse + exactly-one D-06 in-job repair"
  - "generate_json_typed<T: DeserializeOwned> free helper — typed D-07 surface without E0038"
  - "extract_json_block: string-literal-aware brace-depth scanner (fences/prose/nested/unterminated)"
  - "MockProvider + Scripted (Ok/SlowOk/RawText/InvalidJson/Fail/Hang) with call_count/max_concurrent_calls/remaining/prompts accessors"
  - "tests/llm_provider.rs: 9 zero-config tests incl. in-flight concurrency gauge (primitive for plan 02-04)"
affects: [02-04, 02-05, worker-pool, extraction-pipeline, provider-factory]

tech-stack:
  added: []
  patterns:
    - "Dyn-compatible async trait: generic typing lives on a FREE fn (generate_json_typed), never on a trait method; compile-time guard `let _: Option<&dyn LlmProvider> = None;`"
    - "Trait default method implements the cross-provider contract (extract + single repair); impls override only the raw complete() seam"
    - "Scripted test double with AtomicUsize in-flight gauge incremented around the whole body incl. sleeps, so delayed responses are observable"

key-files:
  created:
    - crates/saussunet-core/src/llm/mod.rs
    - crates/saussunet-core/src/llm/mock.rs
    - crates/saussunet-core/tests/llm_provider.rs
  modified:
    - crates/saussunet-core/src/lib.rs

key-decisions:
  - "extract_json_block tries the whole trimmed string first, then scans from first '{' with a brace-depth counter that skips quoted ranges honoring backslash escapes; unterminated brace → None"
  - "repair_prompt embeds original prompt + schema + model's prior output truncated to 4000 chars (chars(), UTF-8-safe) — threat-model token-bomb bound"
  - "MockProvider records every prompt (prompts accessor) so the repair test asserts the 2nd prompt literally contains 'not valid JSON' — plan allowed prompt-recording as an option"
  - "Script exhaustion → CoreError::Llm { status: 0, url: \"mock script exhausted\" } (variant carries no message field by D-03 design; notice rides in url)"
  - "Concurrency gauge test uses tokio::join! over 4 SlowOk(50ms) calls through Arc<dyn LlmProvider> — all 4 enter in_flight before any sleep completes, so max_concurrent_calls()==4 is deterministic on a current-thread runtime"

requirements-completed: [LLMP-01, LLMP-02]

duration: 11min
completed: 2026-10-26
---

# Phase 2 Plan 03: LLM Provider Trait + Deterministic Mock Summary

**Dyn-compatible `LlmProvider` trait whose default `generate_json` runs complete → string-literal-aware `{...}` extraction → exactly one in-job repair prompt, with a scripted `MockProvider` (Ok/SlowOk/RawText/InvalidJson/Fail/Hang) driving typed JSON end-to-end through the free `generate_json_typed<T>` helper — 9 zero-config tests, zero API keys.**

## Performance

- **Duration:** 11 min
- **Started:** 2026-10-26T07:48:40Z
- **Completed:** 2026-10-26T07:59:01Z
- **Tasks:** 2
- **Files modified:** 4 (3 created, 1 modified)

## Accomplishments
- `LlmProvider` is object-safe end-to-end: NON-generic `generate_json(&self, prompt, schema)` with a default body implementing the D-05/D-06 contract (one `complete` → extract → parse; on failure ONE repair `complete`; still bad → `CoreError::LlmParse`), plus a compile-time `&dyn LlmProvider` assertion so a future generic trait method fails the build with E0038. Typed callers use the free `generate_json_typed<T: DeserializeOwned>` (D-07 surface preserved).
- `extract_json_block` handles pure JSON, prose-wrapped, ```json fences, nested objects with braces inside string literals, trailing chatter, and unterminated braces (None) — 6 unit tests.
- `repair_prompt` embeds the model's own prior output bounded to 4000 chars (threat-model token-bomb guard); repair-exhausted test proves exactly 2 calls, zero queue retries (D-06).
- `MockProvider` (scripted VecDeque, tokio::sync::Mutex per vault/mod.rs precedent, NOT Clone, shared as `Arc<dyn LlmProvider>`) overrides only `complete`, so the trait-level repair path is exercised identically to a future HTTP impl. Accessors `call_count`/`max_concurrent_calls`/`remaining`/`prompts` are ready for plan 02-04 and Phase 3.
- D-14 proof holds structurally: `tests/llm_provider.rs` contains no `load_config`, no `FileConfig`, no `OpenAiCompat`, no api_key literal; suite green with `SAUSSUNET_LLM__API_KEY` unset.
- In-flight gauge primitive: 4 concurrent `SlowOk` calls through `Arc<dyn LlmProvider>` yield `max_concurrent_calls() == 4` deterministically (tokio::join! on current-thread runtime) — the bounded-pool assertion seam for plan 02-04.

## Task Commits

1. **Task 1: LlmProvider trait + tolerant parse + repair helpers** — `2096084` (feat)
2. **Task 2: MockProvider (scripted queue) + provider test suite** — `8bf1c55` (test)

**Plan metadata:** this SUMMARY commit (docs)

## Files Created/Modified
- `crates/saussunet-core/src/llm/mod.rs` — `LlmProvider` trait (async_trait), default `generate_json` (D-05/D-06), `generate_json_typed<T>` free helper, `extract_json_block`, `repair_prompt` (4000-char bound), 8 unit tests incl. dyn-compat guard
- `crates/saussunet-core/src/llm/mock.rs` — `Scripted` enum (Ok/SlowOk/RawText/InvalidJson/Fail/Hang), `MockProvider` with prompt recording + atomic call/in-flight/max counters
- `crates/saussunet-core/tests/llm_provider.rs` — 9 tests: script order, fenced-prose extraction, repair success (2nd prompt contains "not valid JSON"), repair-exhausted (exactly 2 calls), Fail-500, Hang, SlowOk, script-exhaustion, concurrency gauge
- `crates/saussunet-core/src/lib.rs` — `pub mod llm;` (factory deliberately NOT added — plan 02-05 owns `provider_from_config`)

## Decisions Made
- `extract_json_block` returns the balanced slice (caller parses); whole-trimmed-string fast path first, then the string-literal-aware brace scan; ASCII delimiters guarantee char-boundary slices.
- Transport errors (`Fail`, exhaustion) propagate immediately from `generate_json` — repair is only for unparseable output, matching D-06's intent.
- `Scripted::InvalidJson` and `RawText` both surface as `Ok(String)` at the `complete` seam; the distinction is test intent (garbage → repair vs noisy-but-valid → extraction).
- `remaining()`/`prompts()` are async (behind the tokio Mutex); `call_count()`/`max_concurrent_calls()` are atomic loads.

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Plan mismatch] Script-exhaustion message rides in `CoreError::Llm.url`**
- **Found during:** Task 2 (MockProvider step())
- **Issue:** Plan said exhaustion → `CoreError::Llm { status: 0, url: "mock" }` "with message 'mock script exhausted'", but the variant (committed in 02-01, D-03 minimalism) has no message field.
- **Fix:** `CoreError::Llm { status: 0, url: "mock script exhausted" }` — Display reads "LLM provider error 0 from mock script exhausted"; rationale comment in mock.rs.
- **Files modified:** crates/saussunet-core/src/llm/mock.rs
- **Verification:** `script_exhaustion_errors_instead_of_hanging` asserts status 0; errors instead of hanging.
- **Committed in:** 8bf1c55 (Task 2 commit)

**2. [Rule 1 - Missing assertion mechanism] MockProvider records prompts**
- **Found during:** Task 2 (repair test)
- **Issue:** Acceptance wants "the 2nd prompt seen by the mock contains 'not valid JSON'"; the plan's pinned struct had no prompt storage ("record prompts via a variant or assert via call_count + success" — neither variant-based option was clean).
- **Fix:** Added `prompts: tokio::sync::Mutex<Vec<String>>` + `pub async fn prompts()`; test asserts the literal substring on the 2nd prompt (stronger than call_count-only).
- **Files modified:** crates/saussunet-core/src/llm/mock.rs, tests/llm_provider.rs
- **Verification:** `repair_prompt_fixes_invalid_json_in_job` green.
- **Committed in:** 8bf1c55 (Task 2 commit)

**3. [Rule 3 - Missing dep] Gauge test uses `tokio::join!` instead of `futures::join_all`**
- **Found during:** Task 2 (compile) — `futures` is not a dev-dependency.
- **Fix:** Four explicit futures + `tokio::join!` on the current-thread runtime; all four enter the in-flight section before any 50 ms sleep completes, making `max_concurrent_calls() == 4` deterministic (no flake window).
- **Files modified:** crates/saussunet-core/tests/llm_provider.rs
- **Verification:** `in_flight_gauge_observes_concurrent_calls` green, repeated runs stable.
- **Committed in:** 8bf1c55 (Task 2 commit)

**4. [Process] `scripts/check.sh` run once at plan end** (same consolidation as plans 02-01/02-02 — it embeds a full release build; per-task verification used `cargo build`/`cargo test`/`cargo clippy --all-targets -D warnings` + acceptance greps).

---

**Total deviations:** 4 (2 minor plan-mismatch adaptations, 1 dep substitution, 1 process consolidation)
**Impact on plan:** No trait/mock semantics changed; every acceptance criterion verified as written. No scope creep — factory (`provider_from_config`) deliberately left to plan 02-05.

## Issues Encountered
- `&Arc<dyn LlmProvider>` does not auto-deref-coerce at the `generate_json_typed` call site and `Arc::clone(&provider)` pinned the wrong type — resolved with `let shared: Arc<dyn LlmProvider> = provider.clone();` + `&*shared`; schema bound to a local to avoid a temporary-dropped-across-futures borrow (E0716).

## Verification Evidence
- `env -u SAUSSUNET_LLM__API_KEY cargo test -p saussunet-core --test llm_provider`: **9 passed, 0 failed** (D-14 zero-key proof)
- `cargo test -p saussunet-core`: **112 passed, 0 failed** (65 lib + 8 + 6 + 9 llm_provider + 1 + 7 + 7 + 9 — was 95 at plan start)
- `cargo clippy -p saussunet-core --all-targets -- -D warnings`: clean; `cargo fmt --check`: clean; `bash scripts/check.sh`: **check: OK** (incl. axum-absence guard + release build)
- Acceptance greps: `pub trait LlmProvider: Send + Sync` / `pub async fn generate_json_typed<T:` / `pub fn extract_json_block` / `Option<&dyn LlmProvider>` / `REPAIR_ECHO_MAX_CHARS: usize = 4000` all present in llm/mod.rs; `pub mod llm;` in lib.rs; `grep -L 'OpenAiCompat\|load_config\|FileConfig' tests/llm_provider.rs` lists the file (tokens absent); `grep -c api_key` → 0; mock.rs contains `pub enum Scripted` with all six variants + `impl LlmProvider for MockProvider`

## Next Phase Readiness
- Plan 02-04 (worker pool): inject `Arc<dyn LlmProvider>` (dyn-compatibility is compile-guarded), use `SlowOk`/`Hang` + `max_concurrent_calls()` for the D-04 bound and D-11 heartbeat/reclaim proofs.
- Plan 02-05: `provider_from_config` factory + `OpenAiCompat` can override `complete` only and inherit the whole D-05/D-06 contract from the trait default.
- Phase 3: `MockProvider` is `pub` for heavy test reuse (CONTEXT.md:94); typed job payloads go through `generate_json_typed`.

## Self-Check: PASSED

---
*Phase: 02-llm-provider-durable-job-queue*
*Completed: 2026-10-26*
