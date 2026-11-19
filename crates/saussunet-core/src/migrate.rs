//! `migrate` — in-place llm-wiki → OKF v0.2 conversion (D-23..D-29).
//!
//! Lives in `saussunet-core` so a host (MoriNet, QUAL-05) can embed the
//! converter (D-28); the binary adds only the clap surface. The conversion
//! reuses the ONE conformance implementation (`vault::okf`, D-21): the same
//! `rewrite_wikilinks` / `rewrite_citations` / `check_page` the write gate
//! uses — one notation everywhere (D-12).
//!
//! Posture (threat model):
//! * dry-run is the default and touches nothing; every mutation in `apply`
//!   runs under `vault.write_lock` through `atomic::write_atomic` ONLY (D-27);
//! * the walk canonicalizes the root once, skips symlinks (reported, never
//!   followed) and re-checks containment under the canonical root;
//! * the report carries relative paths, rule ids, counts and a verdict —
//!   never page bodies or absolute paths (no secret leakage);
//! * every rewritten page gets its `gen_hash` map re-seeded over the FINAL
//!   bytes (the `writer.rs` two-pass trick, Pitfall 2 / D-26) so the next
//!   pipeline merge never reads the conversion as a human edit.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::{CoreError, Result};
use crate::reindex::reindex;
use crate::store::Store;
use crate::vault::{atomic, frontmatter, ghash, log, okf, validate, Vault, PAGE_DIRS};

/// okflint's SKIP_DIRS (`okflint.py:31`) — the migrate walk agrees with the
/// checker walk so nothing converted is invisible to `check_bundle`.
pub(crate) const SKIP_DIRS: [&str; 5] =
    [".git", "scripts", "assets", "__pycache__", "node_modules"];

/// Stable rule ids for the per-rule counts (D-23).
pub mod rules {
    pub const FRONTMATTER: &str = "frontmatter-remap";
    pub const WIKILINKS: &str = "wikilink-rewrite";
    pub const CITATIONS: &str = "citation-rewrite";
    pub const LENIENT: &str = "lenient-parse";
    pub const RAW_SOURCE: &str = "raw-source";
    pub const INDEX_REBUILD: &str = "index-rebuild";
    pub const LOG_REFORMAT: &str = "log-reformat";
    pub const META_TYPE: &str = "meta-type";
}

/// Terminal verdict of a dry-run/apply/verify pass (D-23/D-24).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Dry-run found work to do; nothing was written.
    WouldConvert,
    /// Nothing to change (dry-run) or the conversion completed / verified.
    Clean,
    /// Terminal refusal — no `--force` exists (D-24); zero writes happened.
    Refused { reason: String },
}

impl Verdict {
    /// The refusal reason, when refused.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Verdict::Refused { reason } => Some(reason),
            _ => None,
        }
    }
}

/// One file the conversion would touch (or touched) and the rules applied.
#[derive(Debug, Clone, Serialize)]
pub struct FileChange {
    /// Vault-relative path with forward slashes — never absolute (threat model).
    pub rel: String,
    /// Rule ids applied to this file ([`rules`]).
    pub rules: Vec<String>,
}

/// Machine-readable migration report (D-23): files to change, per-rule
/// counts, skipped protected pages, broken-link INFO count, verdict. Carries
/// relative paths and rule ids ONLY — no page bodies, no absolute paths.
#[derive(Debug, Clone, Serialize)]
pub struct MigrateReport {
    /// Outcome of the pass.
    pub verdict: Verdict,
    /// Files converted (apply) or that would be converted (dry-run).
    pub files_to_change: Vec<FileChange>,
    /// Rule id → number of files the rule fired on.
    pub rule_counts: BTreeMap<String, usize>,
    /// `protected: true` pages skipped and reported (D-26).
    pub skipped_protected: Vec<String>,
    /// Symlinks escaping the vault root, skipped and reported (threat model).
    pub skipped_symlinks: Vec<String>,
    /// INFO telemetry: distinct missing bundle-relative link targets (D-20).
    pub broken_links: usize,
    /// Conformance errors (populated by `verify`).
    pub errors: Vec<String>,
}

impl MigrateReport {
    fn new(skipped_symlinks: Vec<String>) -> Self {
        Self {
            verdict: Verdict::Clean,
            files_to_change: Vec::new(),
            rule_counts: BTreeMap::new(),
            skipped_protected: Vec::new(),
            skipped_symlinks,
            broken_links: 0,
            errors: Vec::new(),
        }
    }

    fn record(&mut self, rel: &str, applied: Vec<String>) {
        for r in &applied {
            *self.rule_counts.entry(r.clone()).or_insert(0) += 1;
        }
        self.files_to_change.push(FileChange {
            rel: rel.to_string(),
            rules: applied,
        });
    }

    /// Human-readable rendering for the CLI (D-23).
    pub fn render_human(&self) -> String {
        let mut out = match &self.verdict {
            Verdict::WouldConvert => format!(
                "dry-run: {} file(s) would be converted\n",
                self.files_to_change.len()
            ),
            Verdict::Clean => "clean: nothing to convert\n".to_string(),
            Verdict::Refused { reason } => format!("REFUSED: {reason}\n"),
        };
        for f in &self.files_to_change {
            out.push_str(&format!("  {} [{}]\n", f.rel, f.rules.join(", ")));
        }
        if !self.rule_counts.is_empty() {
            out.push_str("per-rule counts:\n");
            for (rule, n) in &self.rule_counts {
                out.push_str(&format!("  {rule}: {n}\n"));
            }
        }
        for p in &self.skipped_protected {
            out.push_str(&format!("  skipped protected: {p}\n"));
        }
        for s in &self.skipped_symlinks {
            out.push_str(&format!("  skipped symlink (escapes root): {s}\n"));
        }
        out.push_str(&format!("broken links (info): {}\n", self.broken_links));
        for e in &self.errors {
            out.push_str(&format!("  error: {e}\n"));
        }
        out
    }
}

/// One walked file: vault-relative path + canonical on-disk path.
pub type WalkedFile = (String, PathBuf);

/// Walk `root` for the conversion: every `.md` file, minus `SKIP_DIRS`,
/// minus dot-prefixed entries (in-flight atomic temps), minus symlinks
/// (skipped and reported — never followed, threat model). Every collected
/// path is canonicalized and re-checked to live under the canonical root.
pub fn walk(root: &Path) -> Result<(Vec<WalkedFile>, Vec<String>)> {
    let mut files = Vec::new();
    let mut skipped_symlinks = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).map_err(|e| CoreError::Io {
            path: dir.clone(),
            source: e,
        })?;
        let mut dir_entries: Vec<_> = entries.flatten().collect();
        dir_entries.sort_by_key(|e| e.file_name());
        for entry in dir_entries {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy().to_string();
            if name.starts_with('.') || SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            let ft = entry.file_type().map_err(|e| CoreError::Io {
                path: path.clone(),
                source: e,
            })?;
            if ft.is_symlink() {
                // lstat-based: never followed; reported (threat model).
                skipped_symlinks.push(rel_for(root, &path));
                continue;
            }
            if ft.is_dir() {
                stack.push(path);
                continue;
            }
            if !ft.is_file() || path.extension().is_none_or(|e| e != "md") {
                continue;
            }
            let canonical = path.canonicalize().map_err(|e| CoreError::Io {
                path: path.clone(),
                source: e,
            })?;
            if !canonical.starts_with(root) {
                skipped_symlinks.push(rel_for(root, &path));
                continue;
            }
            files.push((rel_for(root, &path), canonical));
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Ok((files, skipped_symlinks))
}

/// Vault-relative path with forward slashes (report + registry format).
fn rel_for(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Port of `to_okf.py:110-113` `first_h1`: the first `# ` heading's text.
fn first_h1(body: &str) -> String {
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("# ") {
            return rest.trim().to_string();
        }
    }
    String::new()
}

/// Pitfall 6 / PATTERNS §13.11: line-based fallback for hand-written
/// frontmatter that is not valid YAML (`title: "Brother" Jack McDuff`).
/// Port of `to_okf.py:92-108` `parse_lenient` — migrate-only; the writer
/// path keeps the strict parse (tolerant-parse/strict-serialize).
pub fn parse_lenient(yaml_text: &str) -> serde_yml::Mapping {
    let mut out = serde_yml::Mapping::new();
    for line in yaml_text.lines() {
        let l = line.trim_end();
        if l.trim().is_empty() || l.starts_with('#') || l.starts_with(' ') || l.starts_with('\t') {
            continue;
        }
        let Some((key, value)) = l.split_once(':') else {
            continue;
        };
        let value = value.trim();
        let v = if value.starts_with('[') && value.ends_with(']') {
            serde_yml::Value::Sequence(
                value[1..value.len() - 1]
                    .split(',')
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .map(|s| serde_yml::Value::String(s.to_string()))
                    .collect(),
            )
        } else {
            serde_yml::Value::String(value.to_string())
        };
        out.insert(key.trim().to_string(), v);
    }
    out
}

/// Strict parse with the migrate-only lenient fallback (Pitfall 6). Returns
/// the frontmatter plus whether the fallback fired (a reportable rule).
fn parse_fm_lenient(yaml: &str) -> Result<(frontmatter::PageFrontmatter, bool)> {
    match frontmatter::parse(yaml) {
        Ok(fm) => Ok((fm, false)),
        Err(_) => {
            let map = parse_lenient(yaml);
            let text = serde_yml::to_string(&serde_yml::Value::Mapping(map))
                .map_err(|e| CoreError::Frontmatter(format!("lenient re-encode: {e}")))?;
            let fm = frontmatter::parse(&text)?;
            Ok((fm, true))
        }
    }
}

/// Port of `to_okf.py:324-329` `parse_index_descriptions`: old-format
/// `- [[stem]] — desc` entries (alias form included) AND already-converted
/// `- [Title](/path.md) — desc` entries, keyed by target stem.
pub fn parse_index_descriptions(body: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in body.lines() {
        let line = line.trim_start();
        let (target, rest) = if let Some(rest) = line.strip_prefix("- [[") {
            let Some(close) = rest.find("]]") else {
                continue;
            };
            (
                rest[..close].split('|').next().unwrap_or("").to_string(),
                &rest[close + 2..],
            )
        } else if let Some(rest) = line.strip_prefix("- [") {
            let Some(close) = rest.find("](") else {
                continue;
            };
            let after = &rest[close + 2..];
            let Some(end) = after.find(')') else { continue };
            (after[..end].to_string(), &after[end + 1..])
        } else {
            continue;
        };
        let desc = rest.trim().trim_start_matches(['—', '-']).trim();
        if desc.is_empty() {
            continue;
        }
        let key = target
            .rsplit('/')
            .next()
            .unwrap_or("")
            .trim_end_matches(".md")
            .to_string();
        if !key.is_empty() {
            out.insert(key, desc.to_string());
        }
    }
    out
}

/// Port of `to_okf.py:284-321` `reformat_log`: old `## [date] action | subject`
/// headers become `## YYYY-MM-DD` date groups (newest-first) with
/// `* **{Action}**: {subject}` bullets and indented sub-lines. Idempotent on
/// already-converted logs: `## YYYY-MM-DD` groups with `* ` bullets re-emit
/// unchanged, and the `> Format:` line is normalized to the OKF §7 wording.
pub fn reformat_log(md: &str) -> String {
    let lines: Vec<&str> = md.lines().collect();
    let head_end = lines
        .iter()
        .position(|l| l.starts_with("## "))
        .unwrap_or(lines.len());
    let head: Vec<String> = lines[..head_end]
        .iter()
        .map(|l| {
            if l.starts_with("> Format:") {
                "> Format: `## YYYY-MM-DD` headings, most recent first (OKF §7).".to_string()
            } else if l.starts_with("> Chronological record") {
                "> Reverse-chronological record of all wiki actions (newest first).".to_string()
            } else {
                l.to_string()
            }
        })
        .collect();

    // date → blocks; a block is one bullet line plus its indented sub-lines.
    let mut groups: BTreeMap<String, Vec<Vec<String>>> = BTreeMap::new();
    let mut cur: Option<String> = None;
    for line in &lines[head_end..] {
        if let Some(rest) = line.strip_prefix("## ") {
            let rest = rest.trim();
            let bracketed = rest.strip_prefix('[').unwrap_or(rest);
            let candidate: String = bracketed.chars().take(10).collect();
            if okf::log_date_ok(&candidate) {
                let tail = bracketed[10..].trim_start_matches(']').trim();
                let block = if tail.is_empty() {
                    Vec::new()
                } else {
                    let (action, subject) = match tail.split_once('|') {
                        Some((a, s)) => (a.trim(), s.trim()),
                        None => ("", tail),
                    };
                    if action.is_empty() {
                        vec![format!("* {subject}")]
                    } else {
                        let mut chars = action.chars();
                        let cap = match chars.next() {
                            Some(f) => f.to_uppercase().collect::<String>() + chars.as_str(),
                            None => String::new(),
                        };
                        vec![format!("* **{cap}**: {subject}")]
                    }
                };
                cur = Some(candidate.clone());
                groups.entry(candidate).or_default().push(block);
            } else if let Some(d) = &cur {
                // Non-date heading: keep as an indented sub-line (okflint E4
                // will flag it in check_bundle; migrate never drops content).
                if let Some(b) = groups.get_mut(d).and_then(|v| v.last_mut()) {
                    b.push(format!("  {line}"));
                }
            }
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }
        let Some(d) = &cur else { continue };
        let Some(blocks) = groups.get_mut(d) else {
            continue;
        };
        let indented = line.starts_with(' ') || line.starts_with('\t');
        if !indented && line.trim_start().starts_with("* ") {
            blocks.push(vec![line.to_string()]); // already-OKF bullet: new block
        } else {
            let sub = if indented {
                line.to_string()
            } else {
                format!("  {line}")
            };
            match blocks.last_mut() {
                Some(b) => b.push(sub),
                None => blocks.push(vec![sub]),
            }
        }
    }

    let mut out: Vec<String> = head;
    out.push(String::new());
    for (date, blocks) in groups.iter().rev() {
        out.push(format!("## {date}"));
        out.push(String::new());
        for block in blocks {
            if block.is_empty() {
                continue;
            }
            out.extend(block.iter().cloned());
        }
        out.push(String::new());
    }
    let joined = out.join("\n");
    format!("{}\n", joined.trim_end())
}

/// Everything the per-file converters share, built from the tree in one pass.
struct Tree {
    slug_map: HashMap<String, (String, String)>,
    raw_meta: BTreeMap<String, okf::RawSourceMeta>,
    index_desc: HashMap<String, String>,
    /// rel → (title, description fallback) for page-dir files.
    page_info: HashMap<String, (String, String)>,
}

fn is_page_dir(rel: &str) -> bool {
    PAGE_DIRS.contains(&rel.split('/').next().unwrap_or(""))
}

fn stem_of(rel: &str) -> String {
    Path::new(rel)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| rel.to_string())
}

/// Build the shared conversion context: slug map (pages ∪ raw), raw meta
/// (D-13 inputs), index description backfill, per-page title/description.
fn build_tree(files: &[(String, PathBuf, String)]) -> Result<Tree> {
    let mut tree = Tree {
        slug_map: HashMap::new(),
        raw_meta: BTreeMap::new(),
        index_desc: HashMap::new(),
        page_info: HashMap::new(),
    };
    if let Some((_, _, index_body)) = files.iter().find(|(rel, _, _)| rel == "index.md") {
        let (_, body) = frontmatter::split(index_body);
        tree.index_desc = parse_index_descriptions(body);
    }
    for (rel, _, content) in files {
        let is_raw = rel.starts_with("raw/");
        if !is_raw && !is_page_dir(rel) {
            continue;
        }
        let (yaml, body) = frontmatter::split(content);
        let (fm, _lenient) = parse_fm_lenient(yaml)?;
        let stem = stem_of(rel);
        let title = if !fm.title.trim().is_empty() {
            fm.title.trim().to_string()
        } else {
            first_h1(body)
        };
        let title = if title.is_empty() {
            stem.clone()
        } else {
            title
        };
        tree.slug_map
            .insert(stem.clone(), (title.clone(), format!("/{rel}")));
        if is_raw {
            let ingested = fm
                .other
                .get("ingested")
                .and_then(|v| v.as_str())
                .map(okf::to_offset_ts);
            let title = if !fm.title.trim().is_empty() {
                fm.title.trim().to_string()
            } else if !title.is_empty() && title != stem {
                title.clone()
            } else {
                fm.other
                    .get("source_url")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&stem)
                    .to_string()
            };
            tree.raw_meta.insert(
                rel.clone(),
                okf::RawSourceMeta {
                    title,
                    last_modified: ingested,
                },
            );
        } else {
            let desc = fm
                .description
                .as_deref()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| okf::first_sentence(body));
            tree.page_info.insert(rel.clone(), (title, desc));
        }
    }
    Ok(tree)
}

/// Outcome of converting one file.
enum Outcome {
    Changed { page: String, applied: Vec<String> },
    Unchanged,
    Protected,
}

/// Convert one page-dir page: D-06 frontmatter remap (port of
/// `map_page_frontmatter`, to_okf.py:225-245), wikilink rewrite via the
/// shared resolver, footnote citations via the SAME `okf::rewrite_citations`
/// as the writer (D-12), honest `sources[]` (D-13/D-14).
fn convert_page(rel: &str, content: &str, tree: &Tree) -> Result<Option<Outcome>> {
    let (yaml, body) = frontmatter::split(content);
    let (mut fm, lenient) = parse_fm_lenient(yaml)?;
    if fm.protected == Some(true) {
        return Ok(Some(Outcome::Protected));
    }
    let mut applied = vec![rules::FRONTMATTER.to_string()];
    if lenient {
        applied.push(rules::LENIENT.to_string());
    }

    // Type: keep a value already in the emitted vocabulary, else dir-derived
    // (port of to_okf.py `dir_type` via okf::type_for_dir, D-02).
    if !okf::DEFAULT_TYPE_VOCAB.contains(&fm.page_type.as_str()) {
        fm.page_type = okf::type_for_dir(rel).to_string();
    }

    // Wikilinks → markdown links (D-09/D-11), then citations (D-12).
    let body1 = okf::rewrite_wikilinks(body, &tree.slug_map);
    if body1 != body {
        applied.push(rules::WIKILINKS.to_string());
    }
    // sources[] = frontmatter provenance ∪ inline markers, first-appearance
    // order (same union shape as okf::rebuild_frontmatter).
    let mut paths: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for p in fm
        .source_paths()
        .into_iter()
        .chain(validate::extract_provenance(&body1))
    {
        if seen.insert(p.clone()) {
            paths.push(p);
        }
    }
    fm.sources = okf::sources_entries(&paths, &tree.raw_meta);
    let (body2, defs) = okf::rewrite_citations(&body1, &fm.sources);
    if !defs.is_empty() || body2 != body1 {
        applied.push(rules::CITATIONS.to_string());
    }
    let body3 = if defs.is_empty() {
        body2
    } else {
        format!("{}\n\n{}\n", body2.trim_end(), defs)
    };

    // description ← index backfill ∪ existing ∪ first sentence (D-07).
    let stem = stem_of(rel);
    let desc = tree
        .index_desc
        .get(&stem)
        .cloned()
        .filter(|d| !d.trim().is_empty())
        .or_else(|| {
            fm.description
                .as_deref()
                .map(|d| d.trim().to_string())
                .filter(|d| !d.is_empty())
        })
        .unwrap_or_else(|| okf::first_sentence(&body3));
    fm.description = Some(desc);

    // D-08 timestamps: timestamp ← updated, generated.at ← created
    // (date-only values become T00:00:00Z via okf::to_offset_ts).
    let updated = if fm.updated.trim().is_empty() {
        fm.created.trim().to_string()
    } else {
        fm.updated.trim().to_string()
    };
    let created = if fm.created.trim().is_empty() {
        fm.updated.trim().to_string()
    } else {
        fm.created.trim().to_string()
    };
    if !updated.is_empty() {
        fm.timestamp = Some(okf::to_offset_ts(&updated));
    }
    if !created.is_empty() {
        let mut gen = serde_yml::Mapping::new();
        gen.insert(
            "by".to_string(),
            serde_yml::Value::String(okf::actor_string()),
        );
        gen.insert(
            "at".to_string(),
            serde_yml::Value::String(okf::to_offset_ts(&created)),
        );
        fm.generated = Some(serde_yml::Value::Mapping(gen));
    }
    if fm.title.trim().is_empty() {
        fm.title = first_h1(&body3);
        if fm.title.is_empty() {
            fm.title = stem;
        }
    }

    // D-26 / Pitfall 2: two-pass gen_hash re-seed over the FINAL bytes
    // (writer.rs step-6 pattern) so the next merge sees no human edit.
    fm.gen_hash = None;
    let draft = frontmatter::serialize(&fm, &body3);
    let (draft_yaml, draft_body) = frontmatter::split(&draft);
    fm.gen_hash = Some(ghash::compute_map(draft_yaml, draft_body));
    let page = frontmatter::serialize(&fm, &body3);
    Ok(Some(if page == content {
        Outcome::Unchanged
    } else {
        Outcome::Changed { page, applied }
    }))
}

/// Convert one raw record: D-15 `type: Source` block (port of
/// `map_raw_frontmatter`, to_okf.py:248-262); body bytes untouched (D-16).
fn convert_raw(rel: &str, content: &str) -> Result<Option<Outcome>> {
    let (yaml, body) = frontmatter::split(content);
    let (mut fm, lenient) = parse_fm_lenient(yaml)?;
    if fm.protected == Some(true) {
        return Ok(Some(Outcome::Protected));
    }
    let mut applied = vec![rules::RAW_SOURCE.to_string()];
    if lenient {
        applied.push(rules::LENIENT.to_string());
    }
    fm.page_type = "Source".to_string();
    if fm.title.trim().is_empty() {
        let h1 = first_h1(body);
        fm.title = if h1.is_empty() { stem_of(rel) } else { h1 };
    }
    if fm.other.get("resource").is_none() {
        if let Some(url) = fm.other.get("source_url").cloned() {
            fm.other.insert("resource".to_string(), url);
        }
    }
    if let Some(ingested) = fm.other.get("ingested").and_then(|v| v.as_str()) {
        fm.timestamp = Some(okf::to_offset_ts(ingested));
    }
    fm.gen_hash = None;
    let page = frontmatter::serialize(&fm, body);
    Ok(Some(if page == content {
        Outcome::Unchanged
    } else {
        Outcome::Changed { page, applied }
    }))
}

/// Root `SCHEMA.md` / `README.md` get `type: Schema` / `Readme` (Pitfall 5:
/// they count in MoriNet's 80% denominator; B3Wiki precedent).
fn convert_meta(rel: &str, want: &str, content: &str) -> Result<Option<Outcome>> {
    let (yaml, body) = frontmatter::split(content);
    let (fm, _lenient) = parse_fm_lenient(yaml)?;
    if fm.page_type == want {
        return Ok(Some(Outcome::Unchanged));
    }
    let title = if !fm.title.trim().is_empty() {
        fm.title.trim().to_string()
    } else {
        let h1 = first_h1(body);
        if h1.is_empty() {
            stem_of(rel)
        } else {
            h1
        }
    };
    let quoted = serde_yml::to_string(&serde_yml::Value::String(title))
        .unwrap_or_default()
        .trim_end()
        .to_string();
    let page = format!(
        "---\ntype: {want}\ntitle: {quoted}\n---\n\n{}",
        body.trim_start_matches('\n')
    );
    Ok(Some(if page == content {
        Outcome::Unchanged
    } else {
        Outcome::Changed {
            page,
            applied: vec![rules::META_TYPE.to_string()],
        }
    }))
}

/// Remove `^[…]` provenance markers from a short text (index descriptions)
/// — the plain-scan shape of `okf::rewrite_citations` without definitions.
fn strip_citation_markers(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(open) = rest.find("^[") {
        out.push_str(&rest[..open]);
        let after = &rest[open + 2..];
        rest = match after.find(']') {
            Some(close) => &after[close + 1..],
            None => after,
        };
    }
    out.push_str(rest);
    out
}

/// Rebuild the root `index.md` to the D-17/D-18 shape: `okf_version: "0.2"`
/// frontmatter + `- [Title](/path.md) — desc` entries grouped by section,
/// descriptions backfilled from the old index (to_okf.py:324-329). An index
/// that already carries `okf_version` is left untouched.
fn rebuild_index(content: &str, tree: &Tree) -> Outcome {
    let (yaml, _body) = frontmatter::split(content);
    if let Ok(serde_yml::Value::Mapping(m)) = serde_yml::from_str::<serde_yml::Value>(yaml) {
        if m.get("okf_version").is_some() {
            return Outcome::Unchanged;
        }
    }
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let mut entries: Vec<(String, String, String, String)> = Vec::new();
    for (rel, (title, desc)) in &tree.page_info {
        let section = rel.split('/').next().unwrap_or("");
        let section = match section {
            "entities" => "Entities",
            "concepts" => "Concepts",
            "comparisons" => "Comparisons",
            "queries" => "Queries",
            _ => continue,
        };
        // Description backfill (to_okf.py:324-329): old index text wins,
        // else the page's own description/first sentence — sanitized so no
        // retired `[[…]]`/`^[…]` notation lands in the new index.
        let mut desc = tree
            .index_desc
            .get(&stem_of(rel))
            .cloned()
            .filter(|d| !d.trim().is_empty())
            .unwrap_or_else(|| desc.clone());
        desc = okf::rewrite_wikilinks(&desc, &tree.slug_map);
        desc = strip_citation_markers(&desc);
        entries.push((section.to_string(), format!("/{rel}"), title.clone(), desc));
    }
    entries.sort_by(|a, b| a.1.cmp(&b.1));
    let total = entries.len();
    let mut out = String::from("---\nokf_version: \"0.2\"\n---\n# Wiki Index\n\n");
    out.push_str(
        "> Content catalog. Every wiki page listed under its type with a one-line summary.\n",
    );
    out.push_str("> Read this first to find relevant pages for any query.\n");
    out.push_str(&format!("> Last updated: {today} | Total pages: {total}\n"));
    for section in ["Entities", "Concepts", "Comparisons", "Queries"] {
        let in_section: Vec<_> = entries.iter().filter(|e| e.0 == section).collect();
        if in_section.is_empty() {
            continue;
        }
        out.push_str(&format!("\n## {section}\n\n"));
        for (_, path, title, desc) in in_section {
            out.push_str(&format!("- [{title}]({path})"));
            if !desc.trim().is_empty() {
                out.push_str(&format!(" — {}", desc.trim()));
            }
            out.push('\n');
        }
    }
    Outcome::Changed {
        page: out,
        applied: vec![rules::INDEX_REBUILD.to_string()],
    }
}

/// Convert one walked file by its role (port of the `convert` driver,
/// to_okf.py:339-400). `Ok(None)` means the file is out of conversion scope.
fn convert_file(rel: &str, content: &str, tree: &Tree) -> Result<Option<Outcome>> {
    if rel == "index.md" {
        return Ok(Some(rebuild_index(content, tree)));
    }
    if rel == "log.md" {
        let reformatted = reformat_log(content);
        return Ok(Some(if reformatted == content {
            Outcome::Unchanged
        } else {
            Outcome::Changed {
                page: reformatted,
                applied: vec![rules::LOG_REFORMAT.to_string()],
            }
        }));
    }
    if rel == "SCHEMA.md" {
        return convert_meta(rel, "Schema", content);
    }
    if rel == "README.md" {
        return convert_meta(rel, "Readme", content);
    }
    if rel.starts_with("raw/") {
        return convert_raw(rel, content);
    }
    if is_page_dir(rel) {
        return convert_page(rel, content, tree);
    }
    // Stray root files (AGENTS.md, OKF-MAPPING.md, …): treat as pages so the
    // bundle reaches 0 errors (they count in the MoriNet denominator).
    convert_page(rel, content, tree)
}

/// Read the tree, build the shared context (pure reads — used by dry-run).
async fn load_tree(root: &Path) -> Result<(Vec<(String, PathBuf, String)>, Vec<String>)> {
    let (files, skipped_symlinks) = walk(root)?;
    let mut loaded = Vec::with_capacity(files.len());
    for (rel, path) in files {
        let content = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| CoreError::Io {
                path: path.clone(),
                source: e,
            })?;
        loaded.push((rel, path, content));
    }
    Ok((loaded, skipped_symlinks))
}

fn apply_outcome(report: &mut MigrateReport, rel: &str, outcome: Option<Outcome>) {
    match outcome {
        Some(Outcome::Changed { applied, .. }) => report.record(rel, applied),
        Some(Outcome::Protected) => report.skipped_protected.push(rel.to_string()),
        _ => {}
    }
}

fn finish(report: &mut MigrateReport, root: &Path) {
    report.broken_links = okf::count_broken_links(root);
    report.verdict = if report.files_to_change.is_empty() {
        Verdict::Clean
    } else {
        Verdict::WouldConvert
    };
}

/// D-23: dry-run — the full conversion computed in memory, NOTHING written.
pub async fn dry_run(vault: &Vault) -> Result<MigrateReport> {
    let root = vault.root.canonicalize().map_err(|e| CoreError::Io {
        path: vault.root.clone(),
        source: e,
    })?;
    let (files, skipped_symlinks) = load_tree(&root).await?;
    let tree = build_tree(&files)?;
    let mut report = MigrateReport::new(skipped_symlinks);
    for (rel, _, content) in &files {
        let outcome = convert_file(rel, content, &tree)?;
        apply_outcome(&mut report, rel, outcome);
    }
    finish(&mut report, &root);
    Ok(report)
}

/// D-23/D-26/D-27: apply — convert in place under `vault.write_lock` through
/// `atomic::write_atomic` only, re-seed gen_hash over the final bytes, end
/// with a `log.md` migration entry and a full `reindex` so SQLite matches
/// the tree. `backup` gates the undo path (Task 2 refusals).
pub async fn apply(vault: &Vault, store: &Store, backup: Option<&Path>) -> Result<MigrateReport> {
    let root = vault.root.canonicalize().map_err(|e| CoreError::Io {
        path: vault.root.clone(),
        source: e,
    })?;
    let _ = backup; // refusal wiring lands with the Task-2 checks
    let (files, skipped_symlinks) = load_tree(&root).await?;
    let tree = build_tree(&files)?;
    let mut report = MigrateReport::new(skipped_symlinks);
    {
        // Single-writer lock for the whole mutation pass (D-27, VLET-02).
        let _guard = vault.write_lock.lock().await;
        for (rel, path, content) in &files {
            let outcome = convert_file(rel, content, &tree)?;
            match outcome {
                Some(Outcome::Changed { page, applied }) => {
                    atomic::write_atomic(path, page.as_bytes()).await?;
                    report.record(rel, applied);
                }
                Some(Outcome::Protected) => report.skipped_protected.push(rel.clone()),
                _ => {}
            }
        }
        // D-27: dogfood the 04.1-02 bullet log shape for the migration entry.
        let n = report.files_to_change.len();
        log::append_entry(
            vault,
            "migrate-okf",
            &format!("converted {n} file(s) to OKF v0.2 in place"),
            &[],
        )
        .await?;
    }
    // reindex takes the same lock itself — run it AFTER releasing the guard.
    reindex(vault, store).await?;
    finish(&mut report, &root);
    report.verdict = Verdict::Clean;
    Ok(report)
}
