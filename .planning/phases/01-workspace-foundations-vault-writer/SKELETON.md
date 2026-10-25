# Walking Skeleton — SaussuNet

**Phase:** 1
**Generated:** 2026-10-25

> Headless adaptation: Phase 1 ships no web UI (that is Phase 5). The end-to-end slice is:
> binary start → config.toml load → SQLite WAL open + migrations → vault init (SCHEMA/index/log/raw)
> → write one real page through the single atomic writer → read it back / verify on disk.

## Capability Proven End-to-End

> An operator runs the `saussunet` binary and gets a standards-compliant llm-wiki vault created on disk, driven exclusively by one single-writer atomic writer, with human edits protected by per-section `gen_hash` and the SQLite schema ready for many vaults.

Concretely: `cargo run -p saussunet` (no args) loads config, opens SQLite in WAL mode, runs migrations, ensures the vault row, initializes the vault FS (SCHEMA.md, index.md, log.md, raw/), and prints `vault ready: <path>`. `saussunet write-page` then pushes a real page through the writer (frontmatter validation, ≥2 wikilinks, index/log update, page_registry projection, atomic temp+rename), and a human edit to that page survives the next regeneration via the gen_hash three-way check.

## Architectural Decisions

| Decision | Choice | Rationale |
|---|---|---|
| Workspace layout | Cargo workspace: `crates/saussunet-core` (lib, ZERO axum/HTTP deps) + `crates/saussunet` (binary owning all transports), `migrations/` at workspace root | Hard project constraint (INFR-01, Anti-Pattern 1): MoriNet must later depend on the lib without swallowing a web server. Guarded by a grep check in `scripts/check.sh`. |
| Language/runtime | Rust 2021, tokio 1.54 (multi-thread) | Pinned in STACK.md; whole ecosystem (sqlx, later axum/rmcp) runs on it. |
| Persistence | SQLite via **sqlx 0.8** (pinned in `[workspace.dependencies]`), WAL journal + `busy_timeout(5s)` + `foreign_keys(true)` via `SqliteConnectOptions` | **Planner decision (Claude's Discretion):** pin 0.8, not STACK's 0.9. MoriNet's lock pins sqlx 0.8.6; two sqlx majors cannot coexist in one binary; QUAL-05 (core embedded in MoriNet) is the binding future constraint and MoriNet's 0.9 migration is not on our schedule. A later 0.8→0.9 bump is a mechanical migration we control; pinning 0.9 now makes interop hostage to their timeline. Recorded here and in CONVENTIONS at execution time. |
| Data philosophy | Vault FS = sole source of truth; SQLite = rebuildable projection (D-01). No note content in SQLite ever; `reindex` rebuilds `page_registry` from the vault | User-challenged and accepted (01-CONTEXT.md). Losing the DB costs zero content. |
| Single DB, vault-keyed everything | One `data/saussunet.db`; every table carries `vault_id`; vault FS at `data/vaults/<vault_id>/` (D-03/D-04) | Multi-vault-ready (VLET-04) with exactly one vault shipped in v1. |
| Full schema up front | All six tables (`vaults`, `page_registry`, `entity_registry`, `jobs`, `sync_state`, `pending_proposals`) created in Phase 1 migrations; `jobs`/`sync_state` get schema + minimal store APIs only (D-02) | Migrations designed once; behavior lands in Phases 2/4. |
| Vault format | llm-wiki conventions exactly as `/home/c/.hermes/profiles/devo/skills/research/llm-wiki/SKILL.md` (3 layers, frontmatter fields, wikilinks, index/log rules, log rotation at 500 entries) | Format authority (D: Claude's Discretion item 3). Output must be openable in Obsidian unchanged. |
| Write discipline | Single serialized writer (`vault/mod.rs` write lock) + atomic temp+rename in the same directory (`vault/atomic.rs`); writer is the ONLY author of vault files (VLET-02, Pitfall 6) | Retrofit = rewrite; established in the skeleton itself, not hardened in later. |
| Human-edit protection | Per-`##`-section `gen_hash` map in YAML frontmatter, recorded from the FIRST write (D-05); mismatch → skip + `pending_proposals` conflict row + log entry (D-06); `protected: true` → refuse all writes (D-07); no-gen_hash page in `entities/`/`concepts/` → human-owned, refuse (D-08) | Pitfall 7: the hash must exist from day one — it cannot be retrofitted. |
| Operability pre-UI | clap CLI subcommands: `version`, `init`, `init-vault`, `write-page`, `conflicts`, `reindex`; bare run = boot-and-verify (SC1) | **Planner decision (Claude's Discretion):** ROADMAP SC1 only requires "running the binary creates a vault"; no HTTP surface exists before Phase 5, so a CLI is the simplest testable driver and every subcommand is directly assertable in integration tests. |
| Server bind | `[server].bind` defaults to `127.0.0.1:8080` in config; Phase 1 opens NO listener | INFR-02 localhost-default is established at the config layer now; the actual bind arrives with the Phase 5 UI. |
| Error strategy | `thiserror` taxonomy in core (`CoreError`), `anyhow` only at the binary boundary | MoriNet lesson: an untyped anyhow lib cannot be consumed by a future host (PATTERNS §2.9). |
| Deployment target | Single self-hosted binary, no external services (Phase 6 deploys via deploy.sh) | INFR-02; local run command below exercises the full stack. |

## Stack Touched in Phase 1

- [x] Project scaffold (Cargo workspace, `[workspace.dependencies]` pins, `scripts/check.sh` quality gate: fmt + clippy `-D warnings` + test + axum-absence guard)
- [x] "Routing" — adapted for headless: clap subcommand surface (`init`, `init-vault`, `write-page`, `conflicts`, `reindex`)
- [x] Database — real writes (vault row, page_registry upsert, conflict rows) AND real reads (conflicts list, reindex verify), WAL verified by `PRAGMA journal_mode` test
- [x] "UI" — adapted for headless: operator interaction is the CLI driving the writer end-to-end (write a page, edit it by hand, regenerate, see the conflict recorded)
- [x] Deployment — documented local full-stack run: `cargo build --release && ./target/release/saussunet` (creates the vault, exits 0)

## Out of Scope (Deferred to Later Slices)

- Any HTTP server, axum router, web UI, review queue UI (Phase 5)
- LLM provider abstraction and mock (Phase 2); job claiming/retry behavior (Phase 2 — Phase 1 only creates the `jobs` schema + insert/list store APIs)
- Source connectors, watchers, sync_state behavior (Phases 3–4)
- Extraction pipeline, write-path validator for LLM proposals, entity resolution thresholds (Phase 3)
- Lint, log-rotation *triggering* beyond the implemented rule check (Phase 5)
- MCP exposure, mdbook preview, deploy scripts (Phase 6)
- Git auto-commit of the vault (v2, QUAL-01)
- A second real vault (v1.x, SRCX-04) — Phase 1 only makes the schema/paths/APIs vault-id-keyed

## Subsequent Slice Plan

Each later phase adds one vertical slice on top of this skeleton without altering its architectural decisions:

- **Phase 2:** Jobs flow through a crash-safe SQLite queue to workers behind a swappable `LlmProvider` (mock-first) — consumes the `jobs` schema created here.
- **Phase 3:** Submit a URL/PDF → validated, provenance-carrying pages land in the vault through the Phase 1 writer (the writer's gen_hash/protection rules are the gate the pipeline proposes against).
- **Phase 4:** WebDAV watched sources polled on an interval; watcher state persists in `sync_state`; vault updates itself across restarts.
- **Phase 5:** First HTTP surface (localhost bind from config): lint reports, review queue consuming the `pending_proposals` rows Phase 1 records, pipeline monitoring, config from UI.
- **Phase 6:** Read-only MCP endpoint + mdbook preview + deploy to our server.

## Local Full-Stack Run (skeleton proof)

```bash
cargo build --release
./target/release/saussunet                      # bare run: config → WAL SQLite → migrations → vault init → "vault ready: <path>"
./target/release/saussunet write-page --path concepts/walking-skeleton.md --from demo.md
./target/release/saussunet conflicts            # shows gen_hash conflicts after a human edit + regeneration
bash scripts/check.sh                           # fmt + clippy -D warnings + tests + release build
```
