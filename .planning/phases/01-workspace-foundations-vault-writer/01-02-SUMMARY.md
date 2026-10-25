---
phase: 01
plan: 02
subsystem: workspace-foundations-vault-writer
tags: [rust, vault-writer, atomic-write, frontmatter, wikilinks, llm-wiki, sqlite-projection, clap]
key-files:
  - crates/saussunet-core/src/vault/atomic.rs
  - crates/saussunet-core/src/vault/frontmatter.rs
  - crates/saussunet-core/src/vault/writer.rs
  - crates/saussunet-core/src/vault/mod.rs
  - crates/saussunet-core/src/vault/schema.rs
  - crates/saussunet-core/src/vault/index.rs
  - crates/saussunet-core/src/vault/log.rs
  - crates/saussunet-core/src/lib.rs
  - crates/saussunet/src/main.rs
  - crates/saussunet-core/tests/writer.rs
---

# 01-02 Summary: Single atomic vault writer (temp+rename, validation, index/log, write-page CLI)

## What was built

### Task 1 — Atomic primitive + frontmatter module + write lock & path guard (commit fc91c66)
- `vault/atomic.rs`: `write_atomic(path, bytes)` — `create_dir_all` parent, dot-prefixed temp
  `.{name}.tmp-{uuid v4}` in the SAME directory (POSIX-atomic rename), temp removed on any
  failure path, rename last. `append_lines(path, lines)` = read-whole + concat + `write_atomic`
  (one complete entry per atomic write — plain `fs::append` outside the lock is the Pitfall 6 bug).
  Unit tests incl. forced rename failure (non-empty dir at target) proving zero temp leftovers.
- `vault/frontmatter.rs`: `split` (MoriNet never-truncate contract: unterminated `---` block →
  `("", whole input)`), `parse` via serde_yml 0.0.13 into `PageFrontmatter` (title/created/updated/
  type→page_type/tags/sources + optional confidence/contested/contradictions/protected/gen_hash
  BTreeMap + `#[serde(flatten)] other: serde_yml::Mapping` preserving unknown keys),
  `validate_page` (title non-empty; `YYYY-MM-DD` dates via chrono; type ∈ entity|concept|
  comparison|query; tags non-empty and ∈ taxonomy; sources required except query pages — each
  rejection has a distinct message asserted in tests), `serialize` (stable key order, bare dates,
  inline lists, closing `---` on its own line, output re-parses identically),
  `count_outbound_wikilinks` (distinct `[[target]]`, alias/heading suffixes collapsed, `![[embed]]`
  excluded).
- `vault/mod.rs` hardening: `Vault` gains `write_lock: tokio::sync::Mutex<()>` (per-vault,
  VLET-02) and `resolve_page_path(rel)` implementing T-02-01 exactly: reject empty/absolute/`..`
  segment/first-segment `raw` → symlink check on the unresolved joined path → canonicalize deepest
  existing ancestor → assert prefix containment against canonical root. `Vault` is deliberately
  NOT `Clone` anymore (cloning a Mutex would mint a second lock). `root()` accessor added.
  Tests: `../evil.md`, `/tmp/evil.md`, `raw/articles/x.md`, symlinked-file escape, symlinked-dir
  escape, empty path — each asserted by message; plain `concepts/foo.md` accepted.
- `vault/schema.rs`: `parse_tag_taxonomy(vault)` parses `- Category: tag1, tag2` bullets under
  `## Tag Taxonomy` from SCHEMA.md; fails loudly when the section yields no tags.
- `write_if_absent` (init seeding) now routes through `write_atomic` so the plan's grep audit
  (`no fs::write in vault/ outside atomic.rs`) holds across the whole module, tests included.
- Workspace tokio gained the `sync` feature (Mutex).

### Task 2 — write_page pipeline + index/log + projection + CLI (commit e7c09d1)
- `vault/writer.rs`: `WriteOutcome::Written { path }` (conflict variants reserved for 01-03) and
  `write_page(vault, store, rel_path, content)` — THE sole vault mutation entry, doc-commented.
  All inside `vault.write_lock.lock()`: (1) path guard, (2) split/parse/validate against the
  SCHEMA.md taxonomy, (3) wikilink floor ≥2 distinct else `Frontmatter("page requires at least 2
  outbound [[wikilinks]]")`, (4) dates: create → created=updated=today; update → created preserved
  FROM THE EXISTING FILE, updated=today, (5) `write_atomic`, (6) `store.upsert_page` projection
  (metadata only, D-01), (7) `index::upsert_entry`, (8) `log::append_entry` with
  `## [date] create|update | <sanitized title>` + `- wrote <rel>` body. Titles sanitized
  (newlines → space, `## [` → `##[`) against log injection (T-02-05).
- `vault/index.rs`: `upsert_entry` — inserts/replaces `- [[stem]] — title` alphabetically within
  the type section (`## Entities|Concepts|Comparisons|Queries`), rewrites
  `> Last updated: <date> | Total pages: N` counting `- [[` lines across sections; atomic write;
  loud errors if the section or header line is missing.
- `vault/log.rs`: `entry_count` / `should_rotate` (>500) / `fresh_header` / `append_entry` —
  one complete entry per atomic write; rotation moves the old log to `log-<YYYY>.md` (appending
  if that archive already exists) and starts a fresh log.md with the standard header. Rotation
  unit-tested with a 501-entry fixture.
- `lib.rs`: `boot` refactored into `boot_open(cfg) -> (Store, Vault)` (same production sequence);
  `boot` delegates. `main.rs`: `write-page --path <rel> --from <file>` subcommand sharing boot
  wiring; success prints `wrote <rel>` exit 0; `CoreError::Frontmatter`/`Vault` print the cause to
  stderr and exit 1.
- `tests/writer.rs` (7 tests, temp-vault via `boot_open`): happy path (on-disk frontmatter fields
  + both wikilinks + index entry under `## Concepts` + `Total pages: 1` + log `create |` +
  exactly 1 `page_registry` row with correct title/tags_json and no body column via
  `PRAGMA table_info`); 1-wikilink rejection (message + no file); taxonomy rejection (no file);
  writer-level `raw/` and `../` rejection; update path (body replaced, created preserved, updated
  bumped, index not duplicated, second `update |` log entry); log-injection fixture (crafted
  `## [...]` title cannot forge an entry); concurrency — 20 spawned `write_page` calls to 20
  distinct paths → 20 files, index well-formed with counter 20, 20 complete log entries with
  well-formed `## [` shape, 20 registry rows, zero `.tmp-*` anywhere (SC2 / Pitfall 6).

## Commits
| hash | description |
|---|---|
| fc91c66 | feat(01-02): atomic temp+rename primitive, frontmatter parse/validate/serialize, vault write lock and path guard |
| e7c09d1 | feat(01-02): write_page pipeline with index/log updates, page_registry projection, and write-page CLI |

## Deviations
1. **`serialize` hand-composed instead of `serde_yml::to_string(&fm)`**: serde_yml (noyalib, YAML 1.1)
   quotes bare dates (`created: "2026-10-25"`), emits block lists, and omits the trailing newline —
   violating the llm-wiki house style the plan itself pins (bare dates, plain strings). The writer
   now emits llm-wiki-style YAML (bare dates, inline `[a, b]` lists, stable key order, safe-scalar
   heuristic with serde_yml quoting fallback for risky values); a round-trip test proves
   serialize → split → parse is identity. Parsing still uses serde_yml as pinned.
2. **Wikilink counting is a plain hand-rolled scan, not a regex crate**: no `regex` dependency exists
   in the workspace and the plan's intent ("plain regex is the pinned behavior" — i.e. no code-fence
   skipping) is met exactly: distinct `[[target]]` targets, alias/heading suffixes stripped,
   `![[embed]]` excluded. Avoids a new dependency for a 15-line scan.
3. **`Vault` no longer derives `Clone`** (wave 1 had it): `write_lock` must be one lock per vault
   (VLET-02); cloning a `Mutex` would silently mint independent locks. Tests share via `Arc`.
4. **Update preserves `created` from the existing file**, not from the incoming input (plan wording
   "preserve created" — the only reading that makes an update with a stale input date correct;
   asserted by test e).
5. **`write_if_absent` (init seeding) routed through `write_atomic`** so the plan's grep audit
   ("no direct writes anywhere in vault/") holds literally, including test fixtures.
6. **`log rotation` archive appends** if `log-<YYYY>.md` already exists (second rotation in the
   same year) instead of overwriting it — plan silent on this; overwrite would lose history.

## Test results
- `bash scripts/check.sh` → `check: OK` (fmt, `clippy --all-targets -D warnings`, all tests,
  axum-absence guard, release build).
- `cargo test --package saussunet-core` → 41 unit + 5 init_vault + 7 writer = 53 passed, 0 failed.
- Grep audit: `grep -rn "fs::write" crates/saussunet-core/src/vault/ | grep -v atomic.rs` → no
  matches; zero `.tmp-*` files after the full suite.
- Manual release-binary E2E in a temp dir: `init-vault` → `write-page --path concepts/demo.md
  --from demo.md` printed `wrote concepts/demo.md` exit 0; on-disk page has full frontmatter with
  dates normalized to today and both wikilinks; `index.md` has `[[demo]]` under `## Concepts`,
  `Total pages: 1`, `grep -c '[[demo]]' index.md` = 1; `log.md` has the `create | Demo Page`
  entry. Failure case (1 wikilink): stderr `error: frontmatter error: page requires at least 2
  outbound [[wikilinks]]`, exit 1, no file created.

## Decisions (discretion calls made)
- Symlink check runs on the UNRESOLVED joined path before canonicalization, so a symlinked target
  gets the specific "target is a symlink" message while a symlinked parent directory still trips
  the generic "escapes the vault root" prefix check.
- `parse_tag_taxonomy` fails loudly (Vault error) when SCHEMA.md yields no tags — a silently empty
  taxonomy would accept any tag (§2.8 loud-failure posture).
- `boot_open` added to core (not just main.rs) so integration tests drive the exact production
  boot and get the live `(Store, Vault)` pair — same rationale as 01-01's `boot()` placement.
- Index rebuild strategy: entries re-sorted within the section on every upsert (idempotent,
  dedupes by stem prefix); non-entry lines (the alphabetical comment) preserved above entries.
- `sanitize_for_nav` collapses `## [` to `##[` (visible, no invisible characters) — newline
  stripping alone closes the line-injection vector; the collapse defends whole-file scanners.

## Lessons
- serde_yml 0.0.13 (noyalib) is YAML 1.1: it quotes date-like strings on dump and `to_string`
  does not end with a newline — never assume serde_yaml-compatible dump formatting; verify the
  emitted bytes against the format authority. Bare dates DO deserialize back into `String`
  fields, so emitting them bare is round-trip safe.
- `serde_yaml`-style `Mapping::contains_key` takes `&str`, not `&Value` (differs from
  `serde_yaml::Mapping`).
- `#[serde(flatten)]` + declaration order gives stable key order for free on parse, but custom
  serialization (llm-wiki house style) must re-implement that order manually.
- Clippy `-D warnings` catches a self-replace (`.replace("## [", "## [")`) — a sanitizer that
  replaces text with itself is a silent no-op; make the neutralization visible (`##[`).
- Forcing a rename failure in a test: put a non-empty DIRECTORY at the rename target (POSIX
  ENOTEMPTY) — clean way to exercise the temp-cleanup path.
