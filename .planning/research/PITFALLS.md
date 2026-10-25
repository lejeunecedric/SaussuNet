# Pitfalls Research

**Domain:** LLM-maintained knowledge vaults (llm-wiki pattern) with continuous source ingestion — Rust server, Obsidian-compatible output
**Researched:** 2026-10-25
**Confidence:** HIGH (llm-wiki pattern, Google Drive OAuth, WebDAV, MCP spec, entity resolution — all externally corroborated; MoriNet sibling project provides field-tested pitfalls)

> Note: roadmap phases don't exist yet. Phase references below use **provisional phase names** matching the Active requirements in PROJECT.md; remap them when the roadmap is created.

| Provisional phase | Scope |
|---|---|
| P1 — Vault core | `saussunet-core`: vault layout, SCHEMA.md, frontmatter/wikilink model, single-writer FS layer |
| P2 — Ingestion pipeline | Sources → `raw/` → LLM extraction → notes; dedup, link validation |
| P3 — Source watchers | Google Drive, WebDAV, web/PDF polling; sha256 drift tracking |
| P4 — Continuous jobs & scheduling | Job queue, concurrency, incremental update, budgets |
| P5 — MCP in/out | Pointer notes (inbound), vault-as-MCP server (outbound) |
| P6 — Web UI | Config, pipeline monitoring, human review/edit |
| P7 — mdbook preview | Preview build pipeline like MoriNet |

## Critical Pitfalls

### Pitfall 1: Source drift — silent re-ingestion or silent staleness

**What goes wrong:**
Source content changes (Drive file edited, web page updated) and the vault either (a) re-ingests everything on every poll — burning LLM tokens and churning `log.md` — or (b) never notices the change and serves stale notes. Both failure modes feel "working" for weeks.

**Why it happens:**
Pollers are written to list files and diff on mtime only, or to "just re-ingest what's listed." mtime changes without content change (Drive sync, WebDAV server touch) and content changes without mtime change (some servers) both break naive diffing. The llm-wiki pattern itself calls this out — its lint pass includes "source drift sha256" checks — but implementers skip the manifest.

**How to avoid:**
- Maintain a **source manifest** (SQLite): `source_id → sha256(content), etag/last_modified, last_ingested_at, status`.
- Poll cheap first (Drive `changes.list` / `modifiedTime`, WebDAV PROPFIND `getlastmodified`), then hash content, and **only enqueue ingestion when sha256 actually changed**.
- Store the source sha256 in each generated note's frontmatter (`source_sha256:`) so lint can prove which notes are stale.
- Treat `raw/` as content-addressed (e.g. keep previous version or record hash history) so a re-ingest is a diff, not a blind overwrite.

**Warning signs:**
- `log.md` shows repeated `ingest` entries for the same source within days.
- LLM token spend per poll cycle stays flat instead of dropping to ~zero on quiet cycles.
- Users report notes that contradict the live source.

**Phase to address:**
P3 (source watchers) — the manifest and hash-gated enqueue are part of the watcher contract, not a later optimization.

---

### Pitfall 2: Near-duplicate pages / entity duplication

**What goes wrong:**
The LLM creates `React.md`, `React.js.md`, and `reactjs.md` from three different sources. Backlinks scatter across variants, the index bloats, and every downstream page picks a different "canonical" name. This is the single most common failure of agent-maintained wikis.

**Why it happens:**
Each ingestion call sees one source and the index, and name-matches loosely ("React" ≠ "React.js" as a string). There is no canonical entity registry, so the LLM's naming is sampled fresh every time. Entity resolution literature is unambiguous: dedup must be a first-class pipeline stage (blocking → matching → canonical merge), not a hope.

**How to avoid:**
- Keep an **entity registry** (SQLite table): `canonical_name, aliases[], page_path, type`. Before creating any page, the pipeline must resolve the candidate name against it (normalized exact match → fuzzy/embedding similarity).
- Thresholds as a starting point: ≥0.9 similarity auto-link to existing page; 0.7–0.9 route to the human review queue (P6); <0.7 create new. Adjust empirically.
- Run a **periodic dedup pass** (batch candidates in groups of 20–30 into one LLM call — not N pairwise calls) that proposes merges; merges go through review, never silent.
- SCHEMA.md must state naming rules (singular, display name, no version suffixes) and the LLM prompt must include the current index section for the relevant domain.

**Warning signs:**
- Page count grows faster than distinct sources.
- Backlink counts fragment: three pages each with 2–3 inbound links that clearly describe one thing.
- `index.md` sections accumulate near-identical one-liners.

**Phase to address:**
P2 (ingestion pipeline) — entity registry + resolve-before-create is a core pipeline stage; the periodic dedup pass lands with P4 (scheduled jobs).

---

### Pitfall 3: LLM hallucinated links and unsourced content

**What goes wrong:**
The LLM emits `[[Some Page That Doesn't Exist]]`, invents relationships between entities, or "fills in" plausible facts not present in the source. In a vault that is later re-read by the LLM and served over MCP, hallucinations become durable, citable "knowledge."

**Why it happens:**
Wikilinks are free text — nothing validates them at write time. Generation prompts ask for "cross-references" without constraining the link target set. There is no provenance requirement, so the model's parametric knowledge leaks in indistinguishably from source facts.

**How to avoid:**
- **Link whitelist:** the generation prompt receives the candidate link targets (from `index.md` + entity registry) and may only link to those or to explicitly declared planned-stubs. A post-generation validator parses every wikilink and rejects/flags dangling targets before the write happens.
- **Provenance rule:** every generated page carries `source:` (path under `raw/`) + `source_sha256:` in frontmatter; lint flags pages without it. For factual-heavy domains, require section-level source anchors.
- **Never let the LLM write directly to the vault.** Pipeline: generate → validate (links, frontmatter schema, provenance) → write. Validation is deterministic Rust code, not another LLM call.
- Lint pass (llm-wiki style) reports broken links, orphan pages, and pages lacking provenance; surface counts in the P6 UI.

**Warning signs:**
- Broken-link lint count rising pass over pass.
- Pages whose claims can't be found in the cited `raw/` file (spot-check during review).
- Review queue where reviewers rubber-stamp because diffs are too large to check.

**Phase to address:**
P2 (ingestion pipeline) — validator + provenance are part of the write path from day one.

---

### Pitfall 4: Feedback loop — the LLM re-ingests its own notes

**What goes wrong:**
Wiki output leaks back into the input stream: a watcher ingests the vault directory itself, or generation prompts feed existing wiki pages as if they were sources. The vault then amplifies its own past errors — content converges on the model's own phrasing, hallucinations self-cite, and "sources" multiply.

**Why it happens:**
"Update the page about X" naturally means "read the current page about X," and the line between *context* (current page, to be merged into) and *evidence* (raw sources, to be believed) blurs in prompt assembly. Watchers pointed at a parent directory can also swallow the vault output dir.

**How to avoid:**
- Hard architectural rule (already implied by llm-wiki's layering): **only `raw/` is evidence.** The prompt template must separate `EVIDENCE:` (raw sources, with provenance) from `CURRENT PAGE:` (context to merge, explicitly marked as agent-generated, not to be cited).
- Watchers must refuse to ingest any path inside the vault output tree (path-prefix check after canonicalization).
- Frontmatter `generated: true` + `source:` fields make self-citation machine-detectable: lint flags any page whose `source:` resolves inside the wiki layer.
- MCP pointer notes are a special case: they hold *addresses*, never content (PROJECT.md constraint) — enforce by schema-validating pointer-note frontmatter.

**Warning signs:**
- Pages citing other wiki pages as sources.
- Text quality "flattening" over months; distinct sources producing near-identical prose.
- Same claim appearing in more pages without new raw sources.

**Phase to address:**
P1 (vault core — layer rules) and P2 (prompt assembly + lint check).

---

### Pitfall 5: LLM cost/latency blowup — full rebuild instead of incremental update

**What goes wrong:**
Every ingestion re-processes the whole vault (or feeds the entire vault into each prompt). Works fine at 20 pages, unusable at 500: hours-long queues, runaway API bills, or a local Strix Halo model pegged 24/7.

**Why it happens:**
Full rebuild is the simplest correct thing to write first ("regenerate everything, no merge bugs"). The llm-wiki pattern is explicitly incremental — `index.md` is the entry point sized to fit a context window, and an ingest touches only the pages the new source implicates — but implementers default to batch-rebuild habits.

**How to avoid:**
- Design the **incremental contract from day one**: new source → resolve implicated entities via `index.md` + registry → touch only those pages + index + log. Rebuild is an explicit, rare, user-triggered operation.
- **Two-tier model routing:** cheap/local small model for classification, dedup candidates, link suggestion; the bigger model only for page writing/merging. The provider abstraction (trait) should carry a role, not just an endpoint.
- **Budget metering:** track tokens + wall-time per ingestion job; enforce a configurable per-cycle budget; queue degrades gracefully (defer, don't drop silently).
- Cache aggressively: same sha256 → no work; unchanged page sections → don't regenerate.

**Warning signs:**
- Ingestion latency grows linearly (or worse) with vault size.
- Token spend per new source grows as the vault grows.
- You avoid polling more often because "each cycle is expensive."

**Phase to address:**
P2 (incremental update contract) and P4 (budgets, scheduling).

---

### Pitfall 6: Concurrent writers trampling the vault

**What goes wrong:**
Two jobs write the same page (two sources mention the same entity), appends to `log.md` interleave mid-line, or a crash mid-write leaves half-written frontmatter. Obsidian sync then propagates the corruption.

**Why it happens:**
Async Rust makes parallel jobs trivial to spawn (`tokio::spawn` everywhere), and the filesystem has no merge semantics. `log.md` append-only looks safe but isn't under concurrent writers without a lock.

**How to avoid:**
- **Single-writer architecture:** jobs compute in parallel (LLM calls, parsing) but all vault mutations funnel through one writer task/queue. SQLite is already the job store — make it also the write-intent ledger.
- Per-path lock map for the rare case of parallel writes to *different* pages of the same entity; simplest correct v1: serialize writes per vault.
- **Atomic writes:** write to temp file in the same directory + `rename()`. Never truncate-then-write in place.
- `log.md` appends only from the writer task, one complete entry per append.
- If the vault is Obsidian-synced, remember external writers exist too (see Pitfall 7) — the writer task must re-read before write.

**Warning signs:**
- Corrupted frontmatter (unterminated YAML) in the wild.
- `log.md` entries interleaved or duplicated.
- "Lost update": a page loses section B after a job that only touched section A.

**Phase to address:**
P1 (vault core — the FS layer owns writes) — retrofitting single-writer after parallel jobs exist is a rewrite.

---

### Pitfall 7: Human edits silently overwritten

**What goes wrong:**
The user curates a generated page in Obsidian or the review UI; the next ingestion pass regenerates the page from scratch and erases their edits. Users stop trusting (and using) the vault — the product's core value inverts.

**Why it happens:**
The pipeline's mental model is "we own these pages" (llm-wiki says the agent owns the wiki layer). Regeneration is implemented as full-page overwrite because merging is harder. The vault is a plain directory — Obsidian, sync tools, and editors write into it at any time.

**How to avoid:**
- **Three-way merge on write:** frontmatter stores `gen_hash` (hash of the last generated version). Before overwriting, compare current file hash to `gen_hash`: equal → safe overwrite; different → human edited → route the merge to the review queue (P6) instead of clobbering.
- Review UI shows a **diff** (generated proposal vs current page), not just the new content; user can accept, keep-own, or blend.
- Consider a `protected: true` frontmatter flag for pages the user has adopted — the agent may propose but never write.
- Git-init the vault (optional feature): every agent write is a commit with a clear author, making recovery and blame trivial.

**Warning signs:**
- User reports "my notes keep resetting."
- Review queue adoption rate drops (users stop reviewing because it doesn't stick).
- Vault git history (if enabled) shows agent commits deleting user lines.

**Phase to address:**
P1 (store `gen_hash` from the first write) + P6 (review/merge UI). The hash must exist from day one — you can't retroactively know what the user changed.

---

### Pitfall 8: Google Drive OAuth — the seven-day time bomb and token death

**What goes wrong:**
Drive ingestion works perfectly for exactly 7 days, then dies with `invalid_grant: Token has been expired or revoked`. Or: polling gets throttled to a crawl with `userRateLimitExceeded`. Or a re-auth silently issues a new refresh token and invalidates the old one.

**Why it happens:**
Google Cloud projects default to OAuth consent screen status **Testing**, which hard-expires refresh tokens after 7 days for sensitive scopes like `drive.readonly` — no amount of client-side refresh logic can work around it; the grant itself dies. Separately, Drive's default quota is ~10 requests/s per client ID, and there is a 100-live-refresh-tokens-per-account limit where the oldest token is silently revoked.

**How to avoid:**
- **Publish the consent screen to "In production"** (self-use apps <100 users skip Google verification; users click through the unverified warning once). Document this in setup instructions — it's a user-facing setup step, not a dev detail.
- Request the narrowest scope (`drive.readonly`), request refresh token once and **persist it; never re-run interactive auth casually**.
- Handle `invalid_grant` as a distinct, loud state: pipeline pauses that source and the UI shows "reconnect Google Drive" — never a silent retry loop.
- Use delta APIs (`changes.list` with `pageToken`, `drive.files.list` with `fields=` filters) + exponential backoff with jitter on 403/429.

**Warning signs:**
- Fresh setup works, then all Drive sources fail simultaneously ~a week later.
- Poll latency creeping up over hours (backoff accumulation).
- Re-auth "fixes" it for exactly one week.

**Phase to address:**
P3 (source watchers — Drive connector) — token lifecycle states (valid / expiring / revoked → UI action) belong in the connector contract.

---

### Pitfall 9: WebDAV servers are not interchangeable

**What goes wrong:**
The WebDAV watcher works against Nextcloud and explodes against a Synology, an Apache `mod_dav` behind a reverse proxy, or a budget NAS: `PROPFIND Depth: infinity` returns 403 (or DoSes the server), proxies strip PROPFIND with 405, missing trailing slashes cause 301 redirect loops, date formats are non-RFC, and huge trees make listing take minutes.

**Why it happens:**
RFC 4918 leaves latitude and real servers exploit it. The WebDAV working group itself documents `Depth: infinity` PROPFIND as a self-inflicted DoS vector, and many servers (Apache configs, AsterDrive, etc.) refuse it outright. litmus exists as a conformance suite precisely because "compliant" servers diverge. MoriNet already learned this class of lesson (pitfall #13: a date formatter bug only the real server caught — "the tool is the oracle").

**How to avoid:**
- **Never use `Depth: infinity`.** Walk with `Depth: 1` per directory, with a depth cap and per-request timeout.
- Treat 3xx as failure with the host name in the error (don't silently follow redirects across hosts); canonicalize trailing slashes.
- Per-server config knobs (timeout multiplier, whether to trust `getlastmodified`) rather than assuming uniformity; slow-server tolerance: incremental listing, resumable walk.
- Keep a **provider test harness**: run the watcher against at least two real servers in CI (e.g. a Nextcloud container + Apache `mod_dav`), plus litmus-style spot checks — real servers as oracles, not fixtures (MoriNet pitfall #19 lesson).

**Warning signs:**
- One provider works, another returns 405/403 on listing.
- Poll cycles taking minutes; watcher holds connections open for very long.
- Files with special characters (spaces, unicode, `#`) missing from listings.

**Phase to address:**
P3 (source watchers — WebDAV connector) + the CI harness as part of that phase's verification.

---

### Pitfall 10: MCP pointer rot — dead pointers and hung pipelines

**What goes wrong:**
Inbound MCP sources are pointers to remote servers that go down, get renamed, or change tool names. The vault fills with pointer notes whose remote is unreachable; ingestion jobs block on dead servers until timeouts cascade; or worse, the pipeline silently drops dead sources and the user believes coverage exists.

**Why it happens:**
MCP servers are user-run and flaky by nature. The MCP spec says implementations SHOULD set per-request timeouts, issue cancellation notifications, and enforce a max timeout even with progress notifications — but naive clients just `await` forever. Pointer notes have no freshness concept, so rot is invisible.

**How to avoid:**
- Every MCP call: per-request timeout + `notifications/cancelled`, plus an absolute max timeout; distinguish client-side timeout (no response) from server-side error in retry logic. Circuit-breaker per server: N failures → open → stop polling until health check passes.
- Pointer notes carry `mcp_endpoint`, `tool`, `last_verified_at`, `status: ok|stale|dead` in frontmatter. A scheduled health-check pass updates status; **stale ≠ deleted** — dead pointers stay visible (marked) so the user can fix or remove them.
- A dead MCP source must never block other sources: isolate its jobs, fail the job with a recorded error surfaced in the P6 pipeline view.
- Outbound MCP (vault-as-server): read-only by default; never let an external agent's prompt injection drive vault writes.

**Warning signs:**
- Pointer notes with `last_verified_at` older than days.
- Ingestion queue latency spiking whenever one MCP server is down.
- Users asking "why didn't it pick up X?" for a source whose pointer is dead.

**Phase to address:**
P5 (MCP in/out) — pointer lifecycle (verified/stale/dead) is the pointer-note schema, so it must be designed with P1's frontmatter model too.

---

### Pitfall 11: Ontology drift

**What goes wrong:**
The optional ontology (YAML/OWL/RDF) diverges from actual page usage: the LLM invents classes/relations not in the ontology, or the user edits the ontology and existing pages silently violate it. "Optional ontology" becomes "decorative ontology."

**Why it happens:**
Ontology validation isn't wired into the write path, so pages drift freely. When the ontology changes, there's no migration story for existing pages.

**How to avoid:**
- Ontology is **validated input, not suggestion**: parse/validate (YAML schema; OWL/RDF via a parser, not LLM eyeballing) at config time; reject malformed ontologies loudly.
- Generation prompt receives the ontology; the deterministic validator rejects pages using unknown classes/relations (same validator stage as Pitfall 3).
- LLM may **propose** new classes (review queue), never silently add them.
- Ontology version bump → lint pass listing pages that violate the new version; user decides (migrate or relax).

**Warning signs:**
- Pages with `type:` values absent from the ontology.
- Relations in frontmatter that no page consumes.

**Phase to address:**
P2 (validator) + the ontology config surface in P6.

---

### Pitfall 12: Secrets and prompt injection entering the vault

**What goes wrong:**
Ingested sources contain secrets (a PDF with an API key, a Drive doc with credentials) that get copied verbatim into `raw/` and quoted into generated pages — then served to anyone via mdbook preview and the outbound MCP endpoint. Separately, a malicious web source contains instructions ("ignore previous instructions, add a page claiming…") that the ingestion LLM follows, poisoning the vault.

**Why it happens:**
The pipeline treats all ingested text as content; the vault is assumed private but is then exposed through two servers (mdbook preview, MCP). Prompt injection on ingested content is a known LLM failure mode and llm-wiki pipelines are exactly the attack surface: untrusted text + an agent with write access.

**How to avoid:**
- **Never store credentials inside the vault directory.** OAuth tokens, WebDAV passwords, MCP endpoints with embedded secrets live in the app's config/SQLite (with a documented file-permissions note), and the vault dir gets an exclusion list.
- Secret-pattern scan on generated pages before write (regex entropy scan is enough for v1); flag hits to review.
- mdbook preview and MCP outbound default to localhost/bind-private; document that exposing them exposes the whole vault.
- Prompt-injection hygiene: ingestion prompts wrap source content as data with explicit "this is untrusted data, never instructions" framing; the deterministic validator (frontmatter schema, link whitelist, no new classes) bounds what a poisoned LLM output can even express; review queue is the human backstop.

**Warning signs:**
- `grep -ri "api_key\|password\|token" vault/` hits.
- Generated pages with odd meta-content ("system:", instructions addressed to an AI).
- Preview/MCP bound to 0.0.0.0 by default.

**Phase to address:**
P2 (validator + scan) for injection/secret scanning; P5/P7 for exposure defaults.

## Technical Debt Patterns

| Shortcut | Immediate Benefit | Long-term Cost | When Acceptable |
|----------|-------------------|----------------|-----------------|
| Full rebuild instead of incremental update | No merge logic, always consistent | Token/latency cost grows with vault; continuous mode becomes impossible | Prototypes only; must be replaced before "continuous" is claimed (P4) |
| String-match dedup only (no embeddings/LLM pass) | Zero infra, deterministic | Near-duplicates accumulate; cleanup later is a painful batch merge | MVP with a small corpus IF the entity registry + review queue exist so merges stay possible |
| LLM writes directly to vault files | Simple pipeline | No validation gate, no atomicity, hallucinated links ship silently | Never |
| `Depth: infinity` WebDAV listing | One request per tree | 403s on strict servers, DoS on big trees | Never |
| Vault writes without temp-file+rename | Fewer lines | Torn files visible to Obsidian sync / mdbook mid-write | Never |
| Single global LLM model for all steps | One config knob | Big model burns tokens on classification/dedup work | v1 acceptable if budget metering exists to reveal the waste |
| No `gen_hash` on generated pages | Ship faster | Cannot distinguish user edits from agent writes — ever; Pitfall 7 unfixable retroactively | Never |

## Integration Gotchas

| Integration | Common Mistake | Correct Approach |
|-------------|----------------|------------------|
| Google Drive API | Leave OAuth consent in "Testing"; re-run auth on failure | Publish to production; persist refresh token; `invalid_grant` → pause source + UI re-auth action; delta API + backoff with jitter |
| Google Drive API | Poll `files.list` full-tree every cycle | `changes.list` with pageToken + `fields=` projection; ~10 req/s quota budget |
| WebDAV | Assume RFC-uniform servers, use `Depth: infinity`, follow redirects blindly | `Depth: 1` walk with depth cap + timeouts; 3xx = failure; test against ≥2 real servers (litmus-style harness) |
| WebDAV | Trust `getlastmodified` alone for change detection | Hash content before enqueueing ingestion (mtime lies both ways) |
| MCP (inbound) | `await` tool calls with no timeout; treat server as reliable | Per-request timeout + cancellation notification + absolute max timeout (spec SHOULDs); circuit breaker per server; pointer `status` lifecycle |
| MCP (outbound) | Expose vault as read-write MCP for convenience | Read-only by default; writes only via the pipeline/review path |
| mdbook | Assume theme/asset changes appear live (MoriNet pitfall #24) | Themes are written at **build time** and assets may be compile-time constants: rebuild → restart → refresh vault; fingerprinted asset URLs must be resolved from served HTML |
| LLM provider (local vs API) | Hardcode one provider's quirks into the pipeline | Trait carries role (classify vs write) + timeout + budget; local (vLLM/llama.cpp on Strix Halo) and API adapters tested against the same contract |

## Performance Traps

| Trap | Symptoms | Prevention | When It Breaks |
|------|----------|------------|----------------|
| Re-ingest unchanged sources | Flat token spend on idle polls | sha256-gated enqueue (Pitfall 1) | Immediately — wastes money from day 1 |
| Whole-vault context per generation call | Latency ∝ vault size | `index.md` as bounded entry point; fetch only implicated pages | ~100–300 pages (context window dependent) |
| `log.md` parsed as state | Slow startup/lint; giant file in Obsidian | SQLite is the state store; `log.md` is a human-readable append-only render; rotate at ~500 entries → `log-YYYY.md` (llm-wiki convention) | ~6–12 months of continuous operation |
| Broken-link lint over whole vault on every write | Every job O(pages) | Incremental lint on touched pages; full lint as scheduled pass | ~500+ pages |
| Embedding recomputation for dedup | Dedup pass slower each week | Embed pages once, store vectors keyed by content hash; recompute only changed pages | ~1k pages |
| PROPFIND full-tree walk per poll | WebDAV cycle takes minutes | Incremental walk keyed on directory mtime; per-dir caching | Trees >~5k files or slow NAS servers |

## Security Mistakes

| Mistake | Risk | Prevention |
|---------|------|------------|
| OAuth tokens / WebDAV creds stored in vault dir | Served to anyone via mdbook preview or MCP endpoint; synced to Obsidian cloud | Credentials in app config/SQLite outside vault; vault-dir exclusion + startup check that no config file lives inside vault |
| mdbook preview / MCP outbound bound to 0.0.0.0 by default | Full vault disclosure to LAN/internet | Bind localhost by default; any remote exposure is an explicit, logged config change |
| Ingested content treated as instructions (prompt injection) | Vault poisoning via malicious web page / shared doc; poisoned notes then trusted by humans AND external MCP agents | Data/instruction separation in prompts; deterministic output validator bounds damage; review queue for high-risk domains |
| Secrets inside ingested sources copied to `raw/` + quoted into pages | Secret propagation through vault, sync, preview | Pre-write secret scan; review queue flags hits |
| Path traversal via source filenames (Drive/WebDAV names with `../`, percent-encoded) | Write outside vault root | Canonicalize + percent-decode **before** traversal check (MoriNet WebDAV lesson), reject escapes |

## UX Pitfalls

| Pitfall | User Impact | Better Approach |
|---------|-------------|-----------------|
| Silent pipeline failures (source dead, job failed) | User believes vault is current; discovers weeks later | Pipeline view (P6) with per-source health, last-success, error states; pointer `stale/dead` visible in notes |
| Review queue flood after first big ingest | User abandons review → rubber-stamping or ignoring the guardrail | Batch related proposals; prioritize by confidence; allow bulk-accept for low-risk classes (e.g. new entity pages with clean validation) |
| Review shows full new page, not a diff | Impossible to spot what changed on updates to existing pages | Side-by-side diff generated-vs-current; highlight user-edited regions that merge protects |
| "Generated" pages indistinguishable from human notes | Trust erosion; user can't tell what to verify | Frontmatter badge + UI affordance; provenance link back to `raw/` source |
| Re-auth needed but UI shows generic "Drive error" | User restarts everything instead of reconnecting | Distinct actionable state: "Google access expired — reconnect" deep-link |

## "Looks Done But Isn't" Checklist

- [ ] **Drive connector:** often missing production-status setup docs + `invalid_grant` handling — verify by revoking the token and confirming a clean, actionable UI state (not a retry loop).
- [ ] **WebDAV connector:** often missing second-server testing — verify against at least one non-Nextcloud server (Apache mod_dav) and a `Depth: infinity`-refusing server.
- [ ] **MCP pointers:** often missing dead-server behavior — verify pipeline keeps running and marks pointer `stale` when the remote is killed mid-cycle.
- [ ] **Incremental update:** often missing true incrementality — verify a single new source touches <5 pages and token spend stays bounded as vault grows.
- [ ] **Human-edit protection:** often missing the hash check — verify by hand-editing a generated page, triggering re-ingest of its source, and confirming the edit survives or routes to review.
- [ ] **Concurrent writes:** often untested — verify two simultaneous jobs touching the same entity produce no torn files and no lost sections.
- [ ] **mdbook preview:** often missing the rebuild-restart-refresh order (MoriNet #24) — verify a theme change actually reaches a previously-built vault.
- [ ] **Vault hygiene:** often missing lint — verify orphan/broken-link/drift lint runs and reports zero on a fresh ingest of a known corpus.

## Recovery Strategies

| Pitfall | Recovery Cost | Recovery Steps |
|---------|---------------|----------------|
| Source drift missed | MEDIUM | Force re-hash all sources (manifest rebuild), diff against `raw/`, re-ingest changed set |
| Near-duplicate sprawl | HIGH | Batch dedup pass: cluster names (embeddings), LLM merge proposals in groups of 20–30, review, merge pages + rewrite inbound wikilinks to canonical (scriptable — links are text) |
| Hallucinated content shipped | MEDIUM | Lint for pages failing provenance; git history (if vault is git-init'd) to identify bad commits; regenerate affected pages from `raw/` |
| User edits overwritten | HIGH (trust) | Restore from vault git history / Obsidian version history; then implement Pitfall 7 mechanism — trust recovery is slower than data recovery |
| Drive token revoked | LOW | Re-auth flow; delta `pageToken` may be stale → one full-tree rescan, hash-gated so no re-ingest cost |
| Dead MCP pointers accumulated | LOW | Health-check pass marks statuses; user prunes or repairs; no data lost (pointers hold no content) |
| log.md bloat | LOW | Rotate to `log-YYYY.md` at threshold; state lives in SQLite, so nothing depends on it |
| Ontology/pages divergence | MEDIUM | Lint report of violations; user chooses migrate-script or ontology-relax; no silent fix |

## Pitfall-to-Phase Mapping

| Pitfall | Prevention Phase | Verification |
|---------|------------------|--------------|
| Source drift | P3 | Kill-edit a source file; poll detects exactly one re-ingest; unchanged files produce zero jobs |
| Near-duplicates | P2 (+P4 dedup pass) | Ingest corpus with alias variants; entity registry shows one canonical + aliases, one page |
| Hallucinated links/content | P2 | Validator rejects a fixture page with dangling wikilink and missing provenance |
| Feedback loop | P1 + P2 | Watcher refuses vault-internal path; lint flags wiki-as-source provenance |
| Cost/latency blowup | P2 + P4 | Benchmark: token spend for 1 new source at 50 vs 500 pages stays flat |
| Concurrent write corruption | P1 | Two-job same-entity race test; zero torn files, zero lost sections |
| Human edits overwritten | P1 (hash) + P6 (merge UI) | Edit-then-reingest test: edit survives or routes to review |
| Drive OAuth time bomb | P3 | Setup docs include production publish; simulated `invalid_grant` → actionable UI state |
| WebDAV divergence | P3 | CI harness against ≥2 real servers; `Depth: infinity` never emitted (assert in tests) |
| MCP pointer rot | P5 | Timeout + circuit-breaker tests; pointer status lifecycle visible |
| Ontology drift | P2 + P6 | Page using unknown class rejected by validator; ontology change triggers violation lint |
| Secrets / prompt injection | P2 + P5 + P7 | Secret fixture in source → flagged pre-write; preview/MCP bind localhost by default |

## Sources

- Karpathy llm-wiki gist (`gist.github.com/karpathy/442a16...`) — three-layer architecture, log.md append-only + rotation convention, source-drift sha256 lint
- NousResearch `llm-wiki` SKILL.md — log rotation at 500 entries, raw/ immutability, index.md as context-window entry point
- Google OAuth docs (`developers.google.com/identity/protocols/oauth2`) — refresh-token expiry causes, 100-token-per-account limit
- rclone/Drive field reports — 7-day Testing-status refresh-token expiry, ~10 req/s per-client quota, `invalid_grant` failure mode, backoff pacer
- WebDAV RFC 2518/4918 issues list — `Depth: infinity` PROPFIND as DoS vector, servers returning 403; litmus test suite ("a server which passes all tests will not necessarily work with real clients"); AsterDrive compat matrix (infinity → 403 `propfind-finite-depth`); lolly-work WebDAV driver notes (405 from proxies, trailing-slash 301s)
- MCP spec 2025-11-25 Lifecycle — SHOULD timeouts, cancellation notifications, max timeout despite progress notifications; MCP SDK default 60s tool timeout
- Entity resolution literature/practice — blocking→matching→merge pipeline, embedding clustering + batched LLM dedup (KGGen, NeurIPS 2025), 0.9/0.7 threshold heuristic, "entity resolution is never done"
- MoriNet `.planning/HANDOFF.md` + STATE-LOG — pitfall #24 (mdbook themes written at build time, compile-time asset constants, fingerprinted URLs), #19/#13 (real tool as oracle, WebDAV date formatter), #21 (titles are not plain text), WebDAV percent-decode-before-traversal-check

---
*Pitfalls research for: LLM-maintained knowledge vaults with continuous ingestion (SaussuNet)*
*Researched: 2026-10-25*
