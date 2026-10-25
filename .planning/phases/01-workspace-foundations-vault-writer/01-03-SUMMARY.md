---
phase: 01
plan: 03
subsystem: workspace-foundations-vault-writer
tags: [rust, gen-hash, three-way-merge, human-edit-protection, conflicts, llm-wiki, sha256, clap]
key-files:
  - crates/saussunet-core/src/vault/ghash.rs
  - crates/saussunet-core/src/vault/writer.rs
  - crates/saussunet-core/src/store/proposals.rs
  - crates/saussunet-core/src/store/mod.rs
  - crates/saussunet/src/main.rs
  - crates/saussunet-core/tests/gen_hash.rs
---

# 01-03 Summary: Per-section gen_hash three-way merge, conflict recording, conflicts CLI

## What was built

### Task 1 — gen_hash computation + three-way merge inside write_page (commit b6ca746)
- `vault/ghash.rs` (D-05): `split_sections` (sections start at `^## `; `###`+ belong to the
  enclosing section; text before the first heading is the `__preamble__` section; slug =
  kebab-case of heading text, `## Gen Hash Rules!` → `gen-hash-rules`; duplicate headings get
  `-2`/`-3` suffixes so one section can never mask another's hash); `section_hashes` (sha256
  hex per section, exact bytes incl. heading line — trailing *newlines* trimmed at section
  boundaries so an agent appending a new section doesn't falsify the previous section's hash,
  trailing *spaces* kept so whitespace-only human edits are detected); `frontmatter_hash`
  (frontmatter YAML with the `gen_hash` key REMOVED, canonicalized — sorted keys, quoted
  strings, fixed flow rendering — then sha256; identical on write and check sides, T-03-04);
  `compute_map(fm_yaml, body)` = section hashes + `__frontmatter__` key; `human_edited(stored,
  current)` = any stored key changed OR removed (agent-added sections are not human edits).
  9 unit tests incl. the self-hash guard (map embedded in yaml → recompute equals original).
- `vault/writer.rs` three-way check — new step 4 inside `write_page`, under the write lock,
  before any mutation, reading the existing file:
  - D-07 `protected: true` → refuse: conflict row reason `protected`, log `## [date] refused |
    <title> (protected page)`, `WriteOutcome::Refused`.
  - D-06 stored gen_hash map + `human_edited(stored, compute_map(on-disk file))` → SKIP write:
    conflict row reason `gen_hash_mismatch`, log `## [date] conflict | <title>` + `- skipped
    write to <rel> (human edit detected)` + `- proposal recorded (pending_proposals #<id>)`,
    `WriteOutcome::ConflictRecorded { reason, proposal_id }`.
  - D-08 no gen_hash map — including unparseable or missing frontmatter (parse → None) — under
    `entities/`/`concepts/` → refuse reason `human_owned` (a malformed file can never fall
    through to the clean-overwrite branch); outside the wiki layer the rule is scoped away and
    the page is agent-writable.
  - Clean path (first write or matching map): step 6 strips any incoming gen_hash, serializes a
    draft WITHOUT the map, computes `compute_map` over the draft (dates already normalized),
    embeds the map, then serializes the final page — recorded from the FIRST write (Pitfall 7).
  - `WriteOutcome` extended with `ConflictRecorded { reason, proposal_id }` and
    `Refused { reason }`. Reason strings exactly match the 0004 CHECK constraint.
  - Log titles reuse Plan 02's sanitizer; refusal titles fall back to the file stem when the
    existing frontmatter is unparseable (T-03-03).

### Task 2 — conflicts CLI + end-to-end protection tests (commit 40bebe6)
- `store/proposals.rs`: `insert_proposal` already returned `Result<i64>` via
  `last_insert_rowid()` (01-01); added `list_pending_summary(vault_id)` (typed
  `ProposalSummary` rows — id/target_path/reason/created_at, content excluded so the text
  listing never dumps bodies) and `count_pending(vault_id)`; `Proposal`/`ProposalSummary`
  derive `Serialize` for the `--json` path; re-exported both from `store` (submodules were
  private). All queries parameterized (T-03-02).
- `main.rs`: `Conflicts { --json }` subcommand sharing boot wiring — text lines
  `<id>  <reason>  <target_path>  <created_at>`, `no pending conflicts` when empty, pretty
  JSON array (incl. `proposed_content`) with `--json`; exit 0 in both cases. `write-page` now
  maps outcomes: `Written` → `wrote <path>` exit 0; `ConflictRecorded` → `conflict recorded:
  <path> (<reason>)` exit 2; `Refused` → `refused: <path> (<reason>)` exit 2; validation
  errors stay exit 1 (documented in `--help`). No force/bypass flag exists on write-page.
- `tests/gen_hash.rs` (7 tests, temp vault via `boot_open`): (1) **edit survives
  regeneration** — human line still on disk, regenerated line absent, exactly 1 row
  `gen_hash_mismatch` with non-empty proposed_content, `conflict |` log entry, outcome
  carries the row id; (2) clean regeneration — overwrite succeeds, map updated (differs),
  created preserved/updated today, zero conflicts, plus the round-trip guard: stored map ==
  `compute_map` of the file as read back; (3) protected — file bytes byte-identical, row
  `protected`, `refused |` log; (4) human-owned no-gen_hash page under entities/ — refused
  `human_owned`, file untouched; (5) frontmatter-only human edit → `__frontmatter__` mismatch
  routes to conflict; (6) conflicts listing contract — empty list, summary rows, JSON parses
  with proposed_content; (7) malformed-frontmatter page under entities/ → refused
  `human_owned`, bytes byte-identical.

## Commits
| hash | description |
|---|---|
| b6ca746 | feat(01-03): per-section gen_hash computation and three-way merge check in write_page |
| 40bebe6 | feat(01-03): conflicts CLI, proposal listing, and end-to-end human-edit protection tests |

## Deviations
1. **`ghash` uses `serde_yml::Value`, not `serde_yml::yaml::Yaml`** (plan signature):
   serde_yml 0.0.13 (noyalib) has no `yaml::Yaml` module — it exposes the serde_yaml-compat
   `Value`/`Mapping` (String-keyed). Canonicalization implemented over `Value`; `Mapping` keys
   are plain Strings, which simplified `strip_gen_hash`.
2. **`frontmatter_hash` takes `&str` (the YAML region), not a pre-parsed `&Yaml`**: both call
   sites have the raw region; parsing inside keeps the strip-canonicalize-hash rule in one
   place so write and check sides cannot drift.
3. **Section hashes trim trailing newlines at section boundaries** (plan said "exact bytes"):
   the blank line before the next `## ` belongs to the preceding section's slice, so without
   trimming, an agent APPENDING a section would change the previous section's hash and every
   regeneration-with-new-section would falsely conflict. Trailing spaces on content lines are
   still hashed — whitespace-only human edits remain detectable (unit-tested).
4. **Duplicate heading slugs get `-2`/`-3` suffixes** (plan silent): a bare BTreeMap insert
   would let the last section silently mask earlier ones' hashes.
5. **`Proposal`/`ProposalSummary` re-exported from `store`** (`pub use`): store submodules are
   private (01-01 pattern), but the CLI and integration tests must name the row types.
6. **Exit-code mapping tested at the outcome level** (acceptance criterion "helper asserting
   the mapped outcome"): integration tests assert `ConflictRecorded`/`Refused` variants; the
   main.rs mapping to exit 2 is additionally proven by the manual release-binary run below
   (integration tests cannot spawn the bin of a sibling package).

## Test results
- `bash scripts/check.sh` → `check: OK` (fmt, `clippy --all-targets -D warnings`, all tests,
  axum-absence guard, release build).
- `cargo test` → 50 unit + 5 init_vault + 7 writer + **7 gen_hash** = 69 passed, 0 failed.
- `cargo test --package saussunet-core ghash` → 9 unit tests pass (split/nesting/kebab/
  whitespace/human_edited semantics/frontmatter-only detection/self-hash/key-order).
- Grep audit: reason strings in writer.rs are exactly `gen_hash_mismatch` / `protected` /
  `human_owned` (match 0004 CHECK); no force/bypass flag on write-page (only `init` keeps its
  config-template `--force`).
- Manual release-binary E2E (temp dir): `write-page` → `wrote concepts/demo.md` exit 0;
  hand-edit section on disk; regenerate → `conflict recorded: concepts/demo.md
  (gen_hash_mismatch)` **exit 2**; human line still on disk (grep 1), regenerated line absent
  (grep 0); `conflicts` → `1  gen_hash_mismatch  concepts/demo.md  <ts>` exit 0;
  `conflicts --json` → array with `proposed_content`. ROADMAP SC3 TRUE.

## Decisions (discretion calls made)
- Protected check runs BEFORE the gen_hash branch: a protected page refuses even when its
  section hashes match (D-07 says ALL writes).
- Refusal/conflict log titles use the EXISTING page's title (the subject of the refusal),
  falling back to the file stem when its frontmatter is unparseable — sanitized either way.
- The write side computes the map over a draft serialization of the normalized frontmatter
  (dates already bumped), so the stored map equals what the check side will recompute from
  disk — proven by the round-trip guard test.
- `human_edited` treats a stored key missing from current as edited (section removed by human);
  keys only in current are agent additions and ignored.
- `conflicts` text listing omits `proposed_content` (summary query); only `--json` serves it.

## Lessons
- serde_yml 0.0.13 ≠ serde_yaml: no `yaml::Yaml` module; use `serde_yml::Value` and note
  `Mapping` is String-keyed (`FxIndexMap<String, Value>`), so mapping keys are not `Value`s.
- sha2 0.11 `finalize()` output no longer implements `LowerHex` — hex-encode bytes manually.
- Section-boundary hashing must exclude the separator blank line or "append a section" reads
  as a human edit of the previous section; trim trailing newlines only, keep trailing spaces.
- A three-way check must treat "cannot parse" as "cannot prove authorship" — defaulting to
  human-owned, never to the clean-overwrite branch (D-08's real gap was malformed frontmatter).
- Piping cargo through `tail` masks failures in `&&` chains: the commit ran even though clippy
  failed; re-check with the real exit code before committing (fixed via `--amend`).
