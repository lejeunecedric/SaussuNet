---
phase: 4
plan: 01
one_liner: "[[watch]] config surface: WatchSection parse/defaults, resolve() validation into RuntimeConfig.watch, SecretString secrecy, dual template update"
requirements_completed: [SRCC-01]
---

# Plan 04-01 Summary — `[[watch]]` config surface

## What was built

- `crates/saussunet-core/src/config.rs`
  - `WatchSection` (FileConfig side): `url` (required), `username`, `password`
    (`SecretString`), `interval_secs` (default 300, D-10), `depth_cap`
    (default 4, D-04), `extensions` (default `["md","txt","pdf"]`, D-08),
    with manual `Default` impl per the file's discipline and
    `#[serde(default)] pub watch: Vec<WatchSection>` on `FileConfig`.
  - `ResolvedWatch` (RuntimeConfig side): `source_id` = config url VERBATIM
    (state key), parsed `url::Url`, credentials, `interval: Duration`,
    `depth_cap`, normalized `extensions` (lowercased, leading dots stripped).
  - `FileConfig::resolve_watch()` — per-entry validation in plan order:
    url parse → http(s) scheme → userinfo rejection → credential wholeness
    (WR-02 parity) → interval > 0 → depth_cap > 0 → duplicate url
    (source_id collision). Plain `http://` resolves with a
    `tracing::warn!` (D-03: operator-trusted, no SSRF guard). Wired into
    `resolve()` → `RuntimeConfig.watch`.
- `crates/saussunet-core/src/lib.rs` + `config.example.toml` — dual update:
  commented `[[watch]]` example block appended to `default_config()` and the
  committed template in the SAME commit; byte-identity test holds. The block
  also documents the deferred `SAUSSUNET_WATCH__<N>__PASSWORD` env override
  (plan's resolved gap on env parity).

## Tests added (all TDD, RED observed before GREEN)

Task 1 (RED: compile error E0609 `unknown field watch`):
`bare_watch_section_defaults`, `absent_watch_section_is_empty_vec`,
`full_watch_section_parses`.

Task 2 (RED: compile error E0609 `no field watch on RuntimeConfig`):
`zero_interval_rejected`, `zero_depth_cap_rejected`,
`watch_url_rejects_bad_forms` (ftp scheme / userinfo / unparseable),
`watch_requires_whole_credentials`, `duplicate_watch_urls_rejected`,
`watch_resolves_to_runtime_list` (source_id verbatim, `.PDF`→`pdf`,
Duration::from_secs(300), http:// resolves), `watch_password_never_in_debug`
(T12: sentinel absent from RuntimeConfig/FileConfig/section Debug, `***` present).

Final: `cargo test -p saussunet-core` → **217 passed, 0 failed**
(baseline 207 + 10 new). `bash scripts/check.sh` → check: OK.

## Deviations

- `crates/saussunet-core/tests/init_vault.rs` (not in plan files_modified):
  its `RuntimeConfig { ... }` struct literal needed `watch: vec![]` — the new
  field broke compilation of that literal. Committed with Task 2.
- Template block adds two comment lines documenting the deferred env override
  (required by the plan's own resolved gap; block otherwise verbatim from plan).

## Decisions

- Duplicate detection compares the raw config url strings (same source as
  `source_id`), so the collision check matches the state key exactly.
- `resolve_watch` is a private method on `FileConfig`; `resolve()` stays short
  (plan's REFACTOR threshold).

## Lessons

- Adding a field to `RuntimeConfig` breaks every struct literal in integration
  tests — grep `RuntimeConfig {` across `crates/` before adding fields
  (only `init_vault.rs` used a literal; the `rt_for` helpers go through
  `resolve()` and were unaffected).
- `cargo test -p saussunet-core config` filters by test-name path; the watch
  tests live under `config::tests` so `--lib config::` is the precise filter.
