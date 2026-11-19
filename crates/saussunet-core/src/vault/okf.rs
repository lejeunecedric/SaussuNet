//! OKF v0.2 conformance checker + frontmatter builder helpers (D-20/D-21).
//!
//! The single §11 rule implementation — a Rust port of B3Wiki
//! `scripts/okflint.py` (rules E1, E2, E5/W1, W2, W3→E) consumed by the
//! write-path gate (this plan), `migrate-okf --dry-run/--verify` (plan
//! 04.1-05) and the Phase 5 lint. One implementation of conformance, not
//! three (D-21).
//!
//! Never writes anything: every function here is pure over its inputs plus
//! read-only FS reads (mirror of `validate.rs`'s doc discipline). The
//! regex-free scanners follow the `frontmatter::count_outbound_wikilinks`
//! plain-scan precedent (PATTERNS §12 — no regex crate).

use std::collections::BTreeMap;

use serde_yml::Value;

use crate::error::{CoreError, Result};
use crate::store::Store;
use crate::vault::frontmatter;

use super::Vault;

/// Emitted OKF type vocabulary (D-02/D-04). SCHEMA-declared entity subtypes
/// are appended by callers via the `type_vocab` parameter (plan 04.1-04).
pub const DEFAULT_TYPE_VOCAB: [&str; 7] = [
    "Concept",
    "Comparison",
    "Query",
    "Source",
    "Entity",
    "Schema",
    "Readme",
];

/// The §7 actor identity every generated page carries (D-06):
/// `process:saussunet/<CARGO_PKG_VERSION>`.
pub fn actor_string() -> String {
    format!("process:saussunet/{}", env!("CARGO_PKG_VERSION"))
}

/// True iff `s` is ISO 8601 with an explicit UTC offset — the character-scan
/// port of okflint's `ISO_OFFSET` regex
/// `^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:\d{2})$`.
pub fn iso_offset_ok(s: &str) -> bool {
    let b = s.as_bytes();
    // Fixed date-time head: YYYY-MM-DDTHH:MM:SS (19 chars).
    if b.len() < 20 {
        return false;
    }
    let head_ok = (0..19).all(|i| match i {
        4 | 7 => b[i] == b'-',
        10 => b[i] == b'T',
        13 | 16 => b[i] == b':',
        _ => b[i].is_ascii_digit(),
    });
    if !head_ok {
        return false;
    }
    let mut rest = &b[19..];
    // Optional fractional seconds `.digits+`.
    if rest.first() == Some(&b'.') {
        rest = &rest[1..];
        let frac = rest.iter().take_while(|c| c.is_ascii_digit()).count();
        if frac == 0 {
            return false;
        }
        rest = &rest[frac..];
    }
    // Mandatory explicit offset: `Z` or `±HH:MM`.
    if rest == b"Z" {
        return true;
    }
    rest.len() == 6
        && matches!(rest[0], b'+' | b'-')
        && rest[1].is_ascii_digit()
        && rest[2].is_ascii_digit()
        && rest[3] == b':'
        && rest[4].is_ascii_digit()
        && rest[5].is_ascii_digit()
}

/// True iff `s` matches okflint's `ACTOR` shape (W1):
/// `human:<id>` / `process:<id>` (non-space id) or `<producer>/<version>`
/// over `[A-Za-z0-9._-]`. Character-scan port of the regex.
pub fn actor_ok(s: &str) -> bool {
    if let Some(id) = s.strip_prefix("human:").or(s.strip_prefix("process:")) {
        return !id.is_empty() && !id.chars().any(char::is_whitespace);
    }
    let Some((producer, version)) = s.split_once('/') else {
        return false;
    };
    let word_ok = |w: &str| {
        !w.is_empty()
            && w.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    };
    // Exactly one slash: split_once leaves a slash in `version` if there was
    // a second one, and `word_ok` then rejects it.
    word_ok(producer) && word_ok(version)
}

/// D-02 directory → emitted `type` map. `entities/` folds to `Entity` for
/// now (wave-1 placeholder; plan 04.1-04 supplies SCHEMA subtypes).
pub fn type_for_dir(rel: &str) -> &'static str {
    let dir = rel.split('/').next().unwrap_or("");
    match dir {
        "concepts" => "Concept",
        "comparisons" => "Comparison",
        "queries" => "Query",
        "raw" => "Source",
        // entities/ and anything else fold to the generic Entity.
        _ => "Entity",
    }
}

/// D-07 description fallback: first sentence of the body — port of
/// `to_okf.py:82-89` (skip blank and `#`/`>`/`|`/`<!--` lines; first
/// `.!?`-terminated sentence, else the whole line; cap 200 chars).
pub fn first_sentence(body: &str) -> String {
    for line in body.lines() {
        let l = line.trim();
        if l.is_empty()
            || l.starts_with('#')
            || l.starts_with('>')
            || l.starts_with('|')
            || l.starts_with("<!--")
        {
            continue;
        }
        // Lazy first sentence: shortest prefix ending in `.!?` followed by
        // whitespace or end of line (port of `^(.+?[.!?])(?:\s|$)`).
        let mut out = l.to_string();
        let chars: Vec<char> = l.chars().collect();
        for i in 0..chars.len() {
            if ".!?".contains(chars[i]) {
                let ends = i + 1 == chars.len() || chars[i + 1].is_whitespace();
                if ends && i > 0 {
                    out = chars[..=i].iter().collect();
                    break;
                }
            }
        }
        return out.chars().take(200).collect();
    }
    String::new()
}

/// Provenance metadata read off one raw record (D-13 inputs).
#[derive(Debug, Clone)]
pub struct RawSourceMeta {
    /// Raw page title (or its `source_url`, or the path stem as last resort).
    pub title: String,
    /// The raw record's `ingested` timestamp; `None` only when unreadable —
    /// then `last_modified` is honestly omitted instead of invented.
    pub last_modified: Option<String>,
}

/// D-13/D-14: build `sources[]` entries — exactly `{id, resource, title,
/// last_modified}` — from raw targets in first-appearance order. `id` is the
/// raw path stem, `-2`/`-3`-suffixed on collision (port of `to_okf.py:68-74`
/// `uniq`); `resource` is the bundle-relative `/raw/<file>.md`.
pub fn sources_entries(raw_paths: &[String], meta: &BTreeMap<String, RawSourceMeta>) -> Vec<Value> {
    let mut used: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out = Vec::new();
    for path in raw_paths {
        let file = path.rsplit('/').next().unwrap_or(path);
        let stem = file.strip_suffix(".md").unwrap_or(file).to_string();
        // uniq collision suffixing (to_okf.py:68-74).
        let mut id = stem.clone();
        let mut n = 2;
        while !used.insert(id.clone()) {
            id = format!("{stem}-{n}");
            n += 1;
        }
        let m = meta.get(path);
        let mut map = serde_yml::Mapping::new();
        map.insert("id".to_string(), Value::String(id));
        map.insert("resource".to_string(), Value::String(format!("/{path}")));
        map.insert(
            "title".to_string(),
            Value::String(m.map(|x| x.title.clone()).unwrap_or_else(|| stem.clone())),
        );
        if let Some(Some(lm)) = m.map(|x| x.last_modified.clone()) {
            map.insert("last_modified".to_string(), Value::String(lm));
        }
        out.push(Value::Mapping(map));
    }
    out
}

/// The §11 per-page conformance check (D-20/D-21): okflint rules E1, E2,
/// E5/W1, W2, W3→E ported verbatim. Returns one violation string per rule
/// hit (rule id prefixed); an empty vec means the page conforms.
///
/// `type_vocab` gates only the E2 accepted-type set — every caller in this
/// plan passes [`DEFAULT_TYPE_VOCAB`]; plan 04.1-04 passes the union with
/// SCHEMA-declared entity subtypes. Broken cross-links in `body` are
/// counted elsewhere, never rejected here (D-20).
pub fn check_page(fm_yaml: &str, _body: &str, type_vocab: &[&str]) -> Vec<String> {
    let mut v = Vec::new();

    // E1 — missing/unterminated or unparseable frontmatter, or not a mapping.
    if fm_yaml.trim().is_empty() {
        v.push("E1: missing/unterminated YAML frontmatter".to_string());
        return v;
    }
    let parsed: Value = match serde_yml::from_str(fm_yaml) {
        Ok(v) => v,
        Err(e) => {
            v.push(format!("E1: unparseable frontmatter: {e}"));
            return v;
        }
    };
    let fm = match parsed {
        Value::Mapping(m) => m,
        _ => {
            v.push("E1: frontmatter is not a mapping".to_string());
            return v;
        }
    };
    let get = |k: &str| fm.get(k);

    // E2 — non-empty string `type` inside the accepted vocabulary.
    match get("type") {
        Some(Value::String(t)) if !t.trim().is_empty() => {
            if !type_vocab.contains(&t.as_str()) {
                v.push(format!("E2: type {t:?} not in the accepted vocabulary"));
            }
        }
        _ => v.push("E2: frontmatter missing non-empty 'type'".to_string()),
    }

    // W2 — top-level `timestamp` must be an ISO string with explicit offset.
    if let Some(ts) = get("timestamp") {
        match ts {
            Value::String(s) if iso_offset_ok(s) => {}
            Value::String(s) => v.push(format!(
                "W2: timestamp must be ISO 8601 with UTC offset, got {s:?}"
            )),
            _ => v.push("W2: timestamp should be an ISO string".to_string()),
        }
    }

    // E5/W1 — `generated` mapping with a shaped `by` and offset-bearing `at`.
    if let Some(gen) = get("generated") {
        let Value::Mapping(g) = gen else {
            v.push("E5: generated must be a mapping with 'by'".to_string());
            return finish_sources(&mut v, get("sources"));
        };
        let by = g.get("by");
        match by {
            Some(Value::String(b)) if actor_ok(b) => {}
            Some(other) => v.push(format!("W1: generated.by actor shape: {other:?}")),
            None => v.push("E5: generated must be a mapping with 'by'".to_string()),
        }
        if let Some(Value::String(at)) = g.get("at") {
            if !iso_offset_ok(at) {
                v.push(format!(
                    "W2: generated.at must be ISO 8601 with UTC offset, got {at:?}"
                ));
            }
        }
    }

    finish_sources(&mut v, get("sources"))
}

fn finish_sources(v: &mut Vec<String>, sources: Option<&Value>) -> Vec<String> {
    // W3→E — every sources[] entry is a mapping with a non-empty `resource`;
    // W2 — `last_modified` carries an explicit offset.
    if let Some(Value::Sequence(list)) = sources {
        for s in list {
            let entry_ok = matches!(s, Value::Mapping(m)
                if matches!(m.get("resource"),
                    Some(Value::String(r)) if !r.trim().is_empty()));
            if !entry_ok {
                v.push("W3->E: sources[] entries need a resource".to_string());
                continue;
            }
            if let Value::Mapping(m) = s {
                if let Some(Value::String(lm)) = m.get("last_modified") {
                    if !iso_offset_ok(lm) {
                        v.push(format!(
                            "W2: sources[].last_modified must be ISO 8601 with UTC offset, got {lm:?}"
                        ));
                    }
                }
            }
        }
    }
    v.clone()
}

/// D-08 single-source normalization: a bare `YYYY-MM-DD` date becomes
/// `<date>T00:00:00Z` (port of `to_okf.py:51-55` `to_dt`); anything else
/// passes through (a malformed value is then caught by [`check_page`]).
pub fn to_offset_ts(value: &str) -> String {
    if chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok() {
        format!("{value}T00:00:00Z")
    } else {
        value.to_string()
    }
}

/// The write-path OKF rebuild (plan 04.1-01 Task 2): rewrite a
/// gate-passed generated page's frontmatter into D-06 shape — descriptive
/// capitalized `type`, non-empty `description` (D-07 fallback),
/// offset-bearing `timestamp`/`generated` (D-01/D-08), honest `sources[]`
/// (D-13/D-14) — then run [`check_page`] over the rebuilt block. Violations
/// become ONE [`CoreError::Validation`] (validate.rs accumulation pattern).
///
/// Read-only: touches nothing on disk except read-backs of raw records and
/// the existing page (for the drift-free `created` pairing).
pub async fn rebuild_frontmatter(
    vault: &Vault,
    store: &Store,
    rel_path: &str,
    content: &str,
) -> Result<String> {
    let (yaml, body) = frontmatter::split(content);
    let mut fm = frontmatter::parse(yaml)?;

    let mut violations = frontmatter::honesty_violations(&fm);

    // Type: keep a value already in the emitted capitalized vocabulary
    // (registry `page_type` stores lowercase today → tolerant fold to the
    // directory map; plan 04.1-04 wires SCHEMA subtypes through here).
    fm.page_type = if DEFAULT_TYPE_VOCAB.contains(&fm.page_type.as_str()) {
        fm.page_type.clone()
    } else if let Ok(entities) = store.list_entities(vault.id).await {
        entities
            .iter()
            .find(|e| e.page_path.as_deref() == Some(rel_path))
            .and_then(|e| e.page_type.clone())
            .filter(|t| DEFAULT_TYPE_VOCAB.contains(&t.as_str()))
            .unwrap_or_else(|| type_for_dir(rel_path).to_string())
    } else {
        type_for_dir(rel_path).to_string()
    };

    // D-07: never ship an empty description — fall back to the first sentence.
    if fm.description.as_deref().unwrap_or("").trim().is_empty() {
        fm.description = Some(first_sentence(body));
    }

    // D-08 single-source timestamps: `timestamp` from the same value
    // `write_page` will store as `updated` (today), `generated.at` from the
    // same value it will preserve as `created` (existing file's created on
    // update, today on first write) — the pairs cannot drift.
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let existing_created = match tokio::fs::read_to_string(vault.root().join(rel_path)).await {
        Ok(prev) => {
            let (prev_yaml, _) = frontmatter::split(&prev);
            frontmatter::parse(prev_yaml)
                .ok()
                .filter(|p| !p.created.trim().is_empty())
                .map(|p| p.created)
        }
        Err(_) => None,
    };
    let updated_src = if fm.updated.trim().is_empty() {
        today.clone()
    } else {
        fm.updated.clone()
    };
    let created_src = existing_created.unwrap_or_else(|| fm.created.trim().to_string());
    let created_src = if created_src.trim().is_empty() {
        today
    } else {
        created_src
    };
    fm.timestamp = Some(to_offset_ts(&updated_src));
    let mut gen = serde_yml::Mapping::new();
    gen.insert("by".to_string(), Value::String(actor_string()));
    gen.insert("at".to_string(), Value::String(to_offset_ts(&created_src)));
    fm.generated = Some(Value::Mapping(gen));

    // D-13/D-14: sources[] = frontmatter provenance ∪ inline `^[...]`
    // markers, first-appearance order, one entry per raw record.
    let mut paths: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for p in fm
        .source_paths()
        .into_iter()
        .chain(crate::vault::validate::extract_provenance(body))
    {
        if seen.insert(p.clone()) {
            paths.push(p);
        }
    }
    let mut meta = BTreeMap::new();
    for p in &paths {
        meta.insert(p.clone(), read_raw_meta(vault, p).await);
    }
    fm.sources = sources_entries(&paths, &meta);

    let page = frontmatter::serialize(&fm, body);
    let (rebuilt_yaml, _) = frontmatter::split(&page);
    violations.extend(check_page(rebuilt_yaml, body, &DEFAULT_TYPE_VOCAB));
    if !violations.is_empty() {
        return Err(CoreError::Validation(violations.join("; ")));
    }
    Ok(page)
}

/// Read a raw record's frontmatter for D-13 fields (title, ingested).
async fn read_raw_meta(vault: &Vault, rel: &str) -> RawSourceMeta {
    let stem = rel.rsplit('/').next().unwrap_or(rel).to_string();
    let Ok(content) = tokio::fs::read_to_string(vault.root().join(rel)).await else {
        return RawSourceMeta {
            title: stem,
            last_modified: None,
        };
    };
    let (yaml, _) = frontmatter::split(&content);
    let Ok(fm) = frontmatter::parse(yaml) else {
        return RawSourceMeta {
            title: stem,
            last_modified: None,
        };
    };
    let other_str = |k: &str| {
        fm.other
            .get(k)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    };
    let title = if !fm.title.trim().is_empty() {
        fm.title.clone()
    } else {
        other_str("source_url").unwrap_or(stem)
    };
    RawSourceMeta {
        title,
        last_modified: other_str("ingested"),
    }
}

/// okflint E3/W0 (okflint.py:74-87): reserved files (`index.md`, `log.md`)
/// frontmatter rules. `fm_present` = a frontmatter block exists; `fm_keys` =
/// its top-level keys (parsed by the caller — this stays pure). Root
/// `index.md` may carry frontmatter with ONLY `okf_version` (D-17); extra
/// keys are a W0 warning. `log.md` must carry NO frontmatter (E3).
pub fn check_reserved(rel: &str, fm_present: bool, fm_keys: &[String]) -> Vec<String> {
    if !fm_present {
        return Vec::new();
    }
    match rel {
        "log.md" => vec![format!("E3 {rel}: index/log.md must carry no frontmatter")],
        "index.md" => {
            let extra: Vec<&String> = fm_keys.iter().filter(|k| *k != "okf_version").collect();
            if extra.is_empty() {
                Vec::new()
            } else {
                vec![format!(
                    "W0 {rel}: root index frontmatter keys beyond okf_version: {extra:?}"
                )]
            }
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod reserved_tests {
    use super::check_reserved;

    fn keys(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn root_index_okf_version_only_is_clean() {
        assert!(check_reserved("index.md", true, &keys(&["okf_version"])).is_empty());
    }

    #[test]
    fn root_index_extra_keys_warn_w0() {
        let v = check_reserved("index.md", true, &keys(&["okf_version", "title"]));
        assert_eq!(v.len(), 1, "{v:?}");
        assert!(v[0].starts_with("W0"), "{v:?}");
    }

    #[test]
    fn log_with_frontmatter_is_e3() {
        let v = check_reserved("log.md", true, &keys(&["okf_version"]));
        assert_eq!(v.len(), 1, "{v:?}");
        assert!(v[0].starts_with("E3"), "{v:?}");
    }

    #[test]
    fn reserved_without_frontmatter_is_clean() {
        assert!(check_reserved("index.md", false, &[]).is_empty());
        assert!(check_reserved("log.md", false, &[]).is_empty());
    }
}

/// True when `s` is exactly `YYYY-MM-DD` (okflint DATE_OK, regex-free).
pub fn log_date_ok(s: &str) -> bool {
    let r: Vec<char> = s.chars().collect();
    r.len() == 10
        && r[4] == '-'
        && r[7] == '-'
        && r.iter()
            .enumerate()
            .all(|(i, c)| if *c == '-' { i == 4 || i == 7 } else { c.is_ascii_digit() })
}

/// okflint E4 (okflint.py:88-94, LOG_HEADING + DATE_OK): every `## ` heading
/// in a log.md body must be exactly a `## YYYY-MM-DD` date-group heading.
pub fn check_log(body: &str) -> Vec<String> {
    let mut violations = Vec::new();
    for line in body.lines() {
        let Some(rest) = line.strip_prefix("## ") else {
            continue;
        };
        let rest = rest.trim();
        if rest.is_empty() {
            continue; // LOG_HEADING requires a non-space heading body
        }
        if !log_date_ok(rest) {
            violations.push(format!(
                "E4: log date heading must be '## YYYY-MM-DD', got '## {rest}'"
            ));
        }
    }
    violations
}

#[cfg(test)]
mod log_heading_tests {
    use super::check_log;

    #[test]
    fn date_heading_is_clean() {
        assert!(check_log("## 2026-11-18\n* **ingest**: x\n").is_empty());
    }

    #[test]
    fn bracket_heading_is_e4() {
        let v = check_log("## [2026-11-18] create | Wiki\n");
        assert_eq!(v.len(), 1, "{v:?}");
        assert!(v[0].starts_with("E4"), "{v:?}");
    }

    #[test]
    fn non_date_heading_is_e4() {
        let v = check_log("## Notes\n");
        assert_eq!(v.len(), 1, "{v:?}");
        assert!(v[0].starts_with("E4"), "{v:?}");
    }

    #[test]
    fn trailing_text_after_date_is_e4() {
        assert!(check_log("## 2026-11-18 extra").iter().any(|v| v.starts_with("E4")));
    }
}
