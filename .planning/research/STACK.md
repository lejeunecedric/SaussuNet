# Stack Research

**Domain:** Self-hosted Rust server for building/maintaining knowledge vaults (continuous ingestion pipeline + web UI + bidirectional MCP) — same philosophy as MoriNet (single binary, axum, SQLite)
**Researched:** 2026-10-25
**Confidence:** HIGH (versions verified on crates.io on 2026-10-25; a few Medium choices flagged)

## Recommended Stack

### Core Technologies

| Technology | Version | Purpose | Why Recommended | Confidence |
|------------|---------|---------|-----------------|-----------|
| tokio | 1.54 | Async runtime (multi-thread) | De facto standard; the entire selected ecosystem (axum, sqlx, reqwest, rmcp, notify) runs on it. `full` features. | High |
| axum | 0.8.9 | HTTP web framework (API + UI + MCP endpoint) | MoriNet's choice → direct consistency for MoriNet to depend on `saussunet-core`. Tower-native: rmcp's Streamable HTTP service mounts as-is on the router. 543 M downloads. | High |
| tower / tower-http | 0.5.3 / 0.7.1 | Middleware (trace, timeout, compression, fs for static assets) | Natural complement to axum; `ServeDir` serves UI assets without a separate server. | High |
| sqlx (SQLite) | 0.9.0 | Persistence: source state, pipeline jobs, note index, config, reviews | MoriNet's choice. SQLite WAL + one binary = zero ops. ⚠️ MoriNet is on sqlx 0.8: **align the major version in the shared workspace** before MoriNet depends on the lib. | High |
| reqwest | 0.13.5 | Outbound HTTP client (Drive REST, web, Ollama/llama.cpp, MCP) | Standard; use `rustls-tls` (not openssl) for a static binary. | High |
| rmcp (+ rmcp-macros) | 3.7.0 | Official MCP SDK: outbound server (exposed vault) + inbound clients (MCP sources) | Official SDK of the MCP project; implements the 2026-07-28 spec, compatible with 2025-11-25. `StreamableHttpService` is a Tower service mountable on axum; `StreamableHttpClientTransport` client to consume external MCPs. MoriNet already has rmcp experience (Phase 10). | High |
| serde / serde_json / serde_yml | 1.0.154 / 0.0.13 | Serialization (TOML config, YAML frontmatter, YAML ontology, JSON API) | `serde_yml` is the maintained fork of `serde_yaml` (officially deprecated). Essential: note frontmatter + YAML ontology. | High |
| askama + htmx (vendored) | 0.16.1 / htmx 2.x | Web UI: SSR templates + partial interactivity (pipeline tracking, review/editing) | Same philosophy as MoriNet (served HTML + assets, zero node toolchain). askama compiles templates at build time (typed, zero runtime); htmx covers SSE/partials polling without a SPA. Assets vendored in the repo. | Medium |
| rust-embed | 8.13.0 | Embed UI assets into the binary | Single binary `cargo install saussunet`, zero sidecar files. | High |

### Supporting Libraries

| Library | Version | Purpose | When to Use | Confidence |
|---------|---------|---------|-------------|-----------|
| yup-oauth2 | 12.1.2 | OAuth2 on the Google Drive side (user account + refresh token) | The only "Google" piece to integrate. Drive v3 is a simple REST API (`files.list` with `changedSince`/`pageToken`, `files.get?alt=media`) — manual calls via reqwest, no heavy SDK. | High |
| reqwest_dav | 0.3.3 | WebDAV client (PROPFIND/GET, watching remote folders) | Proven async tokio+reqwest client (1.27 M dl, active in 2026). Alternative if insufficient: manual PROPFIND with reqwest + quick-xml 0.42. | Medium |
| pdf_oxide | 0.3.78 | Text extraction from PDFs | Recent pure-Rust PDF library, fastest and best pass-rate on a real corpus (claimed 100% on 3,830 PDFs vs ~91% for pdf_extract); MIT, zero C dependencies. | Medium |
| lopdf | 0.46.0 | Structural PDF manipulation (pages, metadata, images) | Low-level fallback when pdf_oxide doesn't cover a need (text extraction is not built-in). | High |
| scraper + html2md | 0.29.0 / 0.2.17 | Web extraction: CSS selection then HTML→Markdown conversion | Web ingestion pipeline → clean markdown raw text for the LLM. (readability 0.3 is dormant, avoid it.) | Medium |
| pulldown-cmark | 0.13.5 | Markdown parsing: wikilink scan, health lint (orphans, broken links) | Lightweight, fast, events-based — perfect for detecting `[[wikilinks]]` and frontmatter without rewriting the AST. | High |
| comrak | 0.57.0 | Markdown with reversible AST (rewriting notes) | Only if notes need to be rewritten programmatically (otherwise an overload of pulldown-cmark). | Medium |
| gray_matter | 0.3.2 | Parse/write YAML frontmatter (`---`) | Standard Obsidian format; or manual split + serde_yml for full control. | Medium |
| oxigraph + oxrdf + oxttl | 0.5.13 / 0.3.5 / 0.2.5 | RDF ontology: persistent RDF store + data model + Turtle parser | The only serious RDF/SPARQL ecosystem in Rust (the Oxigraph project, actively maintained). For the optional OWL/RDF ontology: store triples, query with SPARQL. No OWL reasoner in Rust — treat OWL as vocabulary + constraints, no DL inference. | Medium |
| git2 | 0.21.0 | Git operations (versioned vaults, sync) | MoriNet's choice (git2 + vendored libgit2) → workspace consistency. gix 0.90 is the pure-Rust alternative to break free of libgit2. | High |
| tokio::process (mdbook CLI) | mdbook 0.5.4 in PATH | mdbook preview of the vault, like MoriNet | Project constraint: mdbook CLI as a subprocess, LOUD failure if missing. `tokio::process::Command` + generating SUMMARY.md/book.toml in a mirror folder (MoriNet `.morinet-build/` pattern). | High |
| notify | 8.2.1 | Filesystem watch (local vault, incoming raw/) | Inotify on Linux; complement with scheduled polling for remote Drive/WebDAV. | High |
| tokio-cron-scheduler | 0.15.1 | Scheduling polls (Drive/WebDAV) and health lint | Only if intervals must be cron-configurable; otherwise `tokio::time::interval` suffices. | Medium |
| clap | 4.6.7 | Binary CLI (`serve`, `init`, `build`, `lint`) | Standard; reproduce the `morinet init` pattern. | High |
| tracing + tracing-subscriber | 0.3.23 | Pipeline observability (spans per ingestion job) | tokio standard; tracing spans also feed UI tracking if hooked to a custom layer. | High |
| thiserror / anyhow | 2.0.21 / 1.0.104 | Typed errors in `saussunet-core`, ergonomic at the binary | thiserror in the lib (MoriNet consumes it), anyhow only at binary boundaries. | High |
| backon | 1.6.0 | Retry/exponential backoff (Drive 429/5xx, WebDAV, LLM) | Drive and LLM APIs demand retry; do it properly from the start. | High |
| chrono / sha2 / uuid / url / dirs / toml | 0.4.45 / 0.11.1 / 1.29 / 2.5.8 / 7.0 / 1.1.8 | Dates, sha256 hashes (llm-wiki source drift), ids, URL parsing, config paths, config.toml | Standard odds and ends, same choices as MoriNet (chrono over time: simpler for the UI). | High |

### LLM Provider (architected choice, not a single crate)

| Approach | Version | Purpose | Why | Confidence |
|----------|---------|---------|-----|-----------|
| Custom `LlmProvider` trait + reqwest over an OpenAI-compatible API | — | Local/cloud abstraction | Ollama **and** llama.cpp server both expose `/v1/chat/completions` (OpenAI-compatible) → a single HTTP client covers Strix Halo (ROCm) + most clouds. Zero framework dependency, full control of retries/timeouts/streaming, compatible with lib mode. | Medium |
| genai (option) | 0.7.1 | Ready-made adapters (Ollama, OpenAI-compatible, Gemini) | If you want off-the-shelf adapters without writing the client; lightweight, active. | Medium |
| rig-core (alternative) | 0.45.0 | Full agents/RAG framework | Choose only if you want rig's agent orchestration; otherwise 0.x churn and unnecessary overhead — the SaussuNet pipeline is a fixed workflow, not a free agent. | Medium |

### Development Tools

| Tool | Purpose | Notes |
|------|---------|-------|
| cargo-dist | Single-binary release + install scripts | MoriNet pattern (publish assets before the release draft→publish); or local `check.sh`/`release.sh` scripts if following MoriNet's latest turn (CI removed, local build). |
| cargo clippy `-D warnings` + cargo fmt | Quality gate | Like MoriNet: clean on every push, gate in `scripts/check.sh`. |
| cargo-insta | Snapshots (generated notes, UI HTML) | Essential to lock the rendering of llm-wiki notes produced by the pipeline. |
| cargo-audit / cargo-deny | Vulnerabilities + licenses + duplicate versions | `cargo-deny` mainly watches for duplicated versions between saussunet-core and MoriNet. |
| mdbook CLI 0.5.4 | Vault preview | Binary in PATH (project constraint), not a cargo dependency. |

## Installation

```toml
# Cargo.toml — workspace: crates/saussunet-core + crates/saussunet-server
[workspace.dependencies]
tokio = { version = "1.54", features = ["full"] }
axum = { version = "0.8", features = ["macros"] }
tower = "0.5"
tower-http = { version = "0.7", features = ["fs", "trace", "timeout", "set-header"] }
sqlx = { version = "0.9", features = ["runtime-tokio", "sqlite", "chrono", "migrate"] }
reqwest = { version = "0.13", default-features = false, features = ["rustls-tls", "json", "stream"] }
rmcp = { version = "3.7", features = ["server", "client", "transport-streamable-http-server", "transport-streamable-http-client-reqwest", "macros"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
serde_yml = "0.0.13"
askama = "0.16"
rust-embed = "8.13"
yup-oauth2 = "12.1"
reqwest_dav = "0.3"
pdf_oxide = "0.3"
lopdf = "0.46"
scraper = "0.29"
html2md = "0.2"
pulldown-cmark = "0.13"
gray_matter = "0.3"
oxigraph = "0.5"
oxrdf = "0.3"
oxttl = "0.2"
git2 = "0.21"
notify = "8.2"
clap = { version = "4.6", features = ["derive"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
thiserror = "2"
anyhow = "1"
backon = "1.6"
chrono = { version = "0.4", features = ["serde"] }
sha2 = "0.11"
uuid = { version = "1.29", features = ["v4", "serde"] }
url = "2.5"
dirs = "7"
toml = "1.1"

[dev-dependencies]
wiremock = "0.6"
insta = "1.49"
assert_cmd = "2.2"
assert_fs = "1.1"
mockall = "0.15"
```

## Alternatives Considered

| Recommended | Alternative | When to Use Alternative |
|-------------|-------------|-------------------------|
| Custom LLM trait + reqwest | genai 0.7 / rig-core 0.45 / async-openai 0.42 | genai if you want adapters without writing code; rig if agent orchestration becomes a product need; async-openai only if 100% OpenAI-compatible and nothing else. ollama-rs 0.3: unnecessary, Ollama's OpenAI-compat API suffices. |
| askama + htmx (SSR) | Leptos 0.8.22 (SSR + wasm hydration) | Leptos if the UI becomes a truly interactive app (live editor, drag & drop of notes). Cost: wasm toolchain, duplicated client/server code — against the MoriNet philosophy. |
| git2 0.21 | gix 0.90 | gix if libgit2 (C) becomes a build/portability problem; pure-Rust, but a more verbose API and MoriNet is already on git2. |
| pdf_oxide | pdf-extract 0.12 / lopdf alone | pdf-extract only if a bug blocks you: lower pass-rate (~91% vs claimed 100%) since it's backed by lopdf. lopdf remains useful for structural work. |
| reqwest_dav | webdav-request 0.4 / manual PROPFIND (quick-xml) | webdav-request if reqwest_dav gets stuck (younger API); manual as a last resort — basic WebDAV protocol fits in 3 requests. |
| Manual Drive REST + yup-oauth2 | google-drive3 7.0 (google-apis-rs) | See "What NOT to Use". If google-cloud-rust ever covers Drive v3, reassess. |
| oxigraph (native RDF) | RDF export via external CLI (Jena, roqet) | Embedded Oxigraph keeps the single binary; an external tool only for OWL inference you actually need. |
| sqlx 0.9 | sqlx 0.8 (MoriNet's version) | If MoriNet must depend on `saussunet-core` **now**, pin sqlx 0.8 during MoriNet's migration — two sqlx majors don't coexist in the same binary. |
| wiremock | httpmock 0.9 | httpmock if you need richer scenarios/mocking on the server mocks side; wiremock is sufficient and more minimal. |

## What NOT to Use

| Avoid | Why | Use Instead |
|-------|-----|-------------|
| google-apis-rs / google-drive3 | The maintainer (Byron) announced in Sep. 2025 the progressive archiving of the project; huge generated bindings, coupled to a dated discovery doc. | Manual Drive v3 REST via reqwest + yup-oauth2 (the needed API fits in ~4 endpoints). |
| `dav-client` (GitHub max246) | **Does not exist on crates.io** (git-only install), blocking synchronous client — incompatible with a tokio server. | reqwest_dav (async, crates.io, maintained). |
| serde_yaml 0.9 | Officially deprecated by its author (March 2024, no more maintenance) — `+deprecated` tag on crates.io. | serde_yml (maintained fork, identical API). |
| pdf-extract as the main extractor | Backed by lopdf, noticeably lower text pass-rate on "hard" real PDFs (~91% vs the 100% claimed by pdf_oxide); a failing PDF = a silent hole in the vault. | pdf_oxide (pure Rust, MIT), lopdf as structural backup. |
| actix-web | Parallel ecosystem incompatible with Tower: rmcp's MCP service and tower-http middlewares don't mount on it; total break with MoriNet. | axum 0.8. |
| diesel | Pointless competition: sqlx is already MoriNet's choice (raw SQL migrations, compile-time queries), and workspace consistency takes priority. | sqlx 0.9 (or aligned 0.8). |
| reqwest with openssl-tls (default feature) | System OpenSSL dependency = non-static binary, deployment headaches. | `default-features = false, features = ["rustls-tls"]`. |
| SPA React/Vue + separate node build | Contradicts the "single binary, zero runtime" contract; node toolchain on the build host. | askama + htmx vendored, assets via rust-embed. |
| rig-core as a hard LLM-abstraction dependency | Fast-churning 0.x framework that imposes its agent model; the SaussuNet pipeline is a deterministic workflow. | Homegrown `LlmProvider` trait + reqwest (genai as an optional shortcut). |
| Homegrown OWL reasoner / SPARQL | No serious OWL reasoner in Rust; reinventing SPARQL is a trap. | oxigraph (embedded SPARQL) + native YAML ontology for 90% of cases; OWL = imported vocabulary, no inference. |
| Homegrown tokio-util `CancellationToken` for the scheduler | No — the real anti-pattern: `std::thread::sleep` threads outside tokio for the polls. | `tokio::time::interval` / tokio-cron-scheduler + `tokio::select!` on shutdown. |

## Stack Patterns by Variant

**If MoriNet must depend on `saussunet-core` in the short term:**
- Pin the common major versions (axum 0.8, sqlx 0.8→0.9 migrated together, reqwest 0.13, tokio 1.x, git2 0.21) in a shared `[workspace.dependencies]` or a documented version contract.
- Because two major versions of the same crate in one binary = incompatible types (immediate error on shared types like `Pool<Sqlite>`).

**If the ontology stays YAML (default case):**
- Don't tie oxigraph to the core of the pipeline: put it behind a `rdf` cargo feature (RDF model → Turtle/NT export on demand).
- Because most vaults won't have OWL; oxigraph's build cost must not weigh on the default path.

**If the local LLM becomes the main path (Strix Halo):**
- Target the OpenAI-compatible API of llama.cpp/Ollama with SSE streaming and `format: json` (structured output); plan a "broken JSON → 1 retry with a repair prompt" fallback.
- Because small local models often break strict JSON — output-parsing robustness matters more than the choice of client crate.

**If the review/edit UI becomes rich (diff, markdown editor):**
- Switch that page alone to Leptos SSR (or CodeMirror vendored + htmx), without migrating the whole UI.
- Because an interactive island costs less than a full-wasm rewrite.

## Version Compatibility

| Package A | Compatible With | Notes |
|-----------|-----------------|-------|
| axum 0.8 | tower 0.5, tower-http 0.7, hyper 1.x | Don't mix with tower 0.4 (axum 0.6/0.7 era). |
| rmcp 3.7 | axum 0.8 (via Tower service), reqwest 0.13 (reqwest client transport) | `StreamableHttpService` is `.nest()`/`.layer()`'d into the axum router; legacy HTTP+SSE (2024-11-05) not supported — older external clients may fail. |
| sqlx 0.9 | MoriNet sqlx 0.8 | ⚠️ Incompatible in the same binary; plan alignment before MoriNet↔SaussuNet integration. |
| reqwest 0.13 | hyper 1.x, rustls | Coexists without issue with axum 0.8 (shared hyper). |
| serde_yml 0.0.13 | serde 1.x | Seemingly unstable 0.0.x versioning but frozen API = clone of serde_yaml 0.9; pin exactly. |
| gray_matter 0.3 | serde_yml (if custom YAML frontmatter) | Verify at implementation time which YAML backend it pins; otherwise manual `---` split. |
| git2 0.21 | vendored libgit2 (default feature) | Build requires a C toolchain (cc); pdf_oxide/reqwest_dav don't. |
| tokio 1.54 | the entire selected ecosystem | A single tokio in the workspace (constrain via `[workspace.dependencies]`). |

## Sources

- crates.io API (verified programmatically on 2026-10-25) — max_stable versions + update dates of ~70 crates (axum 0.8.9, tokio 1.54.1, sqlx 0.9.0, reqwest 0.13.5, rmcp 3.7.0, oxigraph 0.5.13, pdf_oxide 0.3.78, reqwest_dav 0.3.3, askama 0.16.1, etc.). Confidence: High.
- github.com/modelcontextprotocol/rust-sdk (README) — rmcp 3.7: spec 2026-07-28, Streamable HTTP as a Tower service on axum, no legacy HTTP+SSE. Confidence: High.
- github.com/Byron/google-apis-rs discussion #559 (Sep. 2025) — announcement of progressive archiving, recommendation of google-cloud-rust (which doesn't cover Drive v3/Workspace, to verify). Confidence: High on the archiving, Medium on google-cloud-rust coverage.
- pdf.oxide.fyi + pypi.org/project/pdf-oxide — extraction benchmark (claimed by the author: 100% pass rate vs pdf_extract ~91%, lopdf without built-in extraction). Confidence: Medium (partial source, not reproduced).
- crates.io: `dav-client` absent (0 results); serde_yaml marked `+deprecated` (2024-03). Confidence: High.
- MoriNet `.planning/PROJECT.md` — reference stack (axum, sqlx/SQLite, git2, rmcp, mdbook CLI as subprocess, single binary, cargo-dist/local scripts). Confidence: High (internal).

---
*Stack research for: Rust server for building llm-wiki knowledge vaults (continuous ingestion + UI + MCP)*
*Researched: 2026-10-25*
