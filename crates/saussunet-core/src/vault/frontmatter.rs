//! YAML frontmatter split/parse/validate/serialize for llm-wiki pages.
//!
//! Split contract copied in behavior from MoriNet `obsidian/frontmatter.rs`
//! (PATTERNS §2.6): an unterminated `---` block returns the WHOLE input as
//! body — a malformed page is never truncated defensively. Divergence from
//! MoriNet: the YAML body is parsed with serde_yml (nested `gen_hash` maps
//! exceed MoriNet's flat scalar/list subset), and unknown keys are preserved
//! so future fields survive round-trips.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};

/// The four page types the writer accepts (SCHEMA.md `type:` minus `summary`,
/// which Phase 1 does not route to a directory).
pub const PAGE_TYPES: [&str; 4] = ["entity", "concept", "comparison", "query"];

/// Parsed frontmatter of a wiki page. Values stay plain strings (MoriNet
/// rule: no YAML date typing — a bare `2026-10-25` must round-trip as text).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PageFrontmatter {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub created: String,
    #[serde(default)]
    pub updated: String,
    #[serde(rename = "type", default)]
    pub page_type: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub sources: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contested: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contradictions: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protected: Option<bool>,
    /// Writer-managed section-slug → hash map (D-05).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gen_hash: Option<BTreeMap<String, String>>,
    /// Unknown keys are preserved verbatim so future fields survive round-trips.
    #[serde(flatten)]
    pub other: serde_yml::Mapping,
}

/// Split a leading `---\n` frontmatter block from a markdown page.
///
/// Returns `(yaml, body)`. No frontmatter — or an UNTERMINATED block —
/// returns `("", whole input)`: the page is never truncated defensively
/// (MoriNet frontmatter.rs:57-63 contract, threat T-02-04).
pub fn split(md: &str) -> (&str, &str) {
    if !(md.starts_with("---\n") || md.starts_with("---\r\n")) {
        return ("", md);
    }
    let open_end = md.find('\n').map(|i| i + 1).unwrap_or(md.len());
    let mut p = open_end;
    loop {
        let line_end = md[p..].find('\n').map(|i| p + i + 1).unwrap_or(md.len());
        let line = md[p..line_end].trim_end_matches(['\r', '\n']);
        if line == "---" {
            return (&md[open_end..p], &md[line_end..]);
        }
        if line_end == md.len() {
            // No closing delimiter: treat the whole thing as body.
            return ("", md);
        }
        p = line_end;
    }
}

/// Parse the YAML region into [`PageFrontmatter`]. An empty region yields
/// the default struct so [`validate_page`] produces the actionable
/// "title must be non-empty" message instead of a YAML parse error.
pub fn parse(yaml: &str) -> Result<PageFrontmatter> {
    if yaml.trim().is_empty() {
        return Ok(PageFrontmatter::default());
    }
    serde_yml::from_str(yaml)
        .map_err(|e| CoreError::Frontmatter(format!("invalid YAML frontmatter: {e}")))
}

/// Validate against the llm-wiki contract (VLET-01): required fields,
/// `YYYY-MM-DD` dates, page type, taxonomy tags, sources.
pub fn validate_page(fm: &PageFrontmatter, taxonomy: &[String]) -> Result<()> {
    if fm.title.trim().is_empty() {
        return Err(CoreError::Frontmatter(
            "title must be non-empty".to_string(),
        ));
    }
    for (field, value) in [("created", &fm.created), ("updated", &fm.updated)] {
        if NaiveDate::parse_from_str(value, "%Y-%m-%d").is_err() {
            return Err(CoreError::Frontmatter(format!(
                "{field} must be a valid YYYY-MM-DD date, got {value:?}"
            )));
        }
    }
    if !PAGE_TYPES.contains(&fm.page_type.as_str()) {
        return Err(CoreError::Frontmatter(format!(
            "type must be one of {}|{}, got {:?}",
            PAGE_TYPES[..PAGE_TYPES.len() - 1].join("|"),
            PAGE_TYPES[PAGE_TYPES.len() - 1],
            fm.page_type
        )));
    }
    if fm.tags.is_empty() {
        return Err(CoreError::Frontmatter("tags must be non-empty".to_string()));
    }
    for tag in &fm.tags {
        if !taxonomy.iter().any(|t| t == tag) {
            return Err(CoreError::Frontmatter(format!(
                "tag not in SCHEMA.md taxonomy: {tag}"
            )));
        }
    }
    if fm.page_type != "query" && fm.sources.is_empty() {
        return Err(CoreError::Frontmatter(
            "sources must be non-empty except for query pages".to_string(),
        ));
    }
    Ok(())
}

/// Serialize frontmatter + body back into a page in llm-wiki house style:
/// stable key order (title, created, updated, type, tags, sources, then
/// present optional fields, then preserved unknown keys), bare `YYYY-MM-DD`
/// dates and inline lists (MoriNet rule: values stay plain strings — no
/// YAML date typing), closing `---` on its own line.
pub fn serialize(fm: &PageFrontmatter, body: &str) -> String {
    let mut out = String::from("---\n");
    out.push_str(&format!("title: {}\n", scalar(&fm.title)));
    out.push_str(&format!("created: {}\n", scalar(&fm.created)));
    out.push_str(&format!("updated: {}\n", scalar(&fm.updated)));
    out.push_str(&format!("type: {}\n", scalar(&fm.page_type)));
    out.push_str(&format!("tags: {}\n", flow(&fm.tags)));
    out.push_str(&format!("sources: {}\n", flow(&fm.sources)));
    if let Some(c) = &fm.confidence {
        out.push_str(&format!("confidence: {}\n", scalar(c)));
    }
    if let Some(v) = fm.contested {
        out.push_str(&format!("contested: {v}\n"));
    }
    if let Some(list) = &fm.contradictions {
        out.push_str(&format!("contradictions: {}\n", flow(list)));
    }
    if let Some(v) = fm.protected {
        out.push_str(&format!("protected: {v}\n"));
    }
    if let Some(map) = &fm.gen_hash {
        out.push_str("gen_hash:\n");
        for (k, v) in map {
            out.push_str(&format!("  {}: {}\n", scalar(k), scalar(v)));
        }
    }
    if !fm.other.is_empty() {
        let mut extra = serde_yml::to_string(&fm.other).expect("preserved keys must re-serialize");
        if !extra.ends_with('\n') {
            extra.push('\n');
        }
        out.push_str(&extra);
    }
    out.push_str("---\n");
    // The blank line after the closing delimiter belongs to the body per
    // the split contract — do not double it when body already starts one.
    if !body.starts_with('\n') {
        out.push('\n');
    }
    out.push_str(body);
    out
}

/// Plain scalar when safe (llm-wiki house style keeps dates bare), quoted
/// via serde_yml when the value could confuse a YAML parser.
fn scalar(s: &str) -> String {
    let safe = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || " ._-/".contains(c))
        && !s.ends_with(' ')
        && !matches!(
            s.to_ascii_lowercase().as_str(),
            "true" | "false" | "yes" | "no" | "on" | "off" | "null" | "~"
        );
    if safe {
        s.to_string()
    } else {
        serde_yml::to_string(&s.to_string())
            .expect("string must serialize")
            .trim_end()
            .to_string()
    }
}

/// Inline flow list (`[a, b]`) per the llm-wiki frontmatter template.
fn flow(items: &[String]) -> String {
    let inner: Vec<String> = items.iter().map(|i| scalar(i)).collect();
    format!("[{}]", inner.join(", "))
}

/// Count DISTINCT `[[target]]` wikilink targets in `body` (VLET-01 floor is
/// 2 outbound links). Plain scan is the pinned behavior — code-fence skipping
/// is overkill here. `[[target|alias]]` and `[[target#heading]]` count as
/// `target`; `![[image.png]]` embeds are not outbound page links.
pub fn count_outbound_wikilinks(body: &str) -> usize {
    let mut targets = std::collections::HashSet::new();
    let mut rest = body;
    while let Some(open) = rest.find("[[") {
        // An embed `![[...]]` is an image, not an outbound page link.
        let is_embed = open > 0 && rest.as_bytes()[open - 1] == b'!';
        let after = &rest[open + 2..];
        let Some(close) = after.find("]]") else {
            break;
        };
        if !is_embed {
            let target = after[..close].split(['|', '#']).next().unwrap_or("").trim();
            if !target.is_empty() {
                targets.insert(target.to_string());
            }
        }
        rest = &after[close + 2..];
    }
    targets.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn taxonomy() -> Vec<String> {
        ["technology", "science", "person", "process"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    fn valid_fm() -> PageFrontmatter {
        PageFrontmatter {
            title: "Walking Skeleton".to_string(),
            created: "2026-10-25".to_string(),
            updated: "2026-10-25".to_string(),
            page_type: "concept".to_string(),
            tags: vec!["technology".to_string()],
            sources: vec!["raw/articles/x.md".to_string()],
            ..Default::default()
        }
    }

    #[test]
    fn split_extracts_block() {
        let (yaml, body) = split("---\ntitle: X\n---\nbody text\n");
        assert_eq!(yaml, "title: X\n");
        assert_eq!(body, "body text\n");
    }

    #[test]
    fn split_unterminated_block_returns_whole_input_as_body() {
        let md = "---\nno close";
        let (yaml, body) = split(md);
        assert_eq!(yaml, "");
        assert_eq!(body, md);
    }

    #[test]
    fn split_no_frontmatter() {
        let (yaml, body) = split("# just markdown");
        assert_eq!(yaml, "");
        assert_eq!(body, "# just markdown");
    }

    #[test]
    fn parse_round_trips_and_preserves_unknown_keys() {
        let yaml = "title: X\ncreated: 2026-10-25\nupdated: 2026-10-25\ntype: concept\ntags: [technology]\nsources: []\nfuture_field: kept\n";
        let fm = parse(yaml).unwrap();
        assert_eq!(fm.title, "X");
        assert_eq!(fm.page_type, "concept");
        assert_eq!(fm.tags, vec!["technology".to_string()]);
        assert!(fm.other.contains_key("future_field"));
    }

    #[test]
    fn validate_accepts_good_frontmatter() {
        validate_page(&valid_fm(), &taxonomy()).unwrap();
    }

    #[test]
    fn validate_rejects_tag_outside_taxonomy() {
        let mut fm = valid_fm();
        fm.tags = vec!["nonsense".to_string()];
        let err = validate_page(&fm, &taxonomy()).unwrap_err().to_string();
        assert!(
            err.contains("tag not in SCHEMA.md taxonomy: nonsense"),
            "{err}"
        );
    }

    #[test]
    fn validate_rejects_unknown_page_type() {
        let mut fm = valid_fm();
        fm.page_type = "note".to_string();
        let err = validate_page(&fm, &taxonomy()).unwrap_err().to_string();
        assert!(err.contains("type must be one of"), "{err}");
        assert!(err.contains("note"), "{err}");
    }

    #[test]
    fn validate_rejects_bad_date() {
        let mut fm = valid_fm();
        fm.updated = "2026-13-99".to_string();
        let err = validate_page(&fm, &taxonomy()).unwrap_err().to_string();
        assert!(
            err.contains("updated must be a valid YYYY-MM-DD date"),
            "{err}"
        );
    }

    #[test]
    fn validate_rejects_empty_title() {
        let mut fm = valid_fm();
        fm.title = "  ".to_string();
        let err = validate_page(&fm, &taxonomy()).unwrap_err().to_string();
        assert!(err.contains("title must be non-empty"), "{err}");
    }

    #[test]
    fn validate_requires_sources_except_query_pages() {
        let mut fm = valid_fm();
        fm.sources.clear();
        let err = validate_page(&fm, &taxonomy()).unwrap_err().to_string();
        assert!(err.contains("sources must be non-empty"), "{err}");
        fm.page_type = "query".to_string();
        validate_page(&fm, &taxonomy()).unwrap();
    }

    #[test]
    fn serialize_has_stable_key_order_and_full_frontmatter() {
        let out = serialize(&valid_fm(), "body\n");
        assert!(out.starts_with("---\ntitle: Walking Skeleton\n"));
        let pos = |k: &str| out.find(k).expect(k);
        assert!(pos("title:") < pos("created:"));
        assert!(pos("created:") < pos("updated:"));
        assert!(pos("updated:") < pos("type: concept"));
        assert!(pos("type:") < pos("tags:"));
        assert!(pos("tags:") < pos("sources:"));
        assert!(out.contains("---\n\nbody\n"));
    }

    #[test]
    fn serialize_output_reparses_identically() {
        let fm = valid_fm();
        let page_out = serialize(&fm, "body\n");
        let (yaml, body) = split(&page_out);
        let fm2 = parse(yaml).unwrap();
        assert_eq!(fm, fm2, "writer output must re-parse identically");
        // Per the split contract the blank line after the closing `---`
        // belongs to the body, and serialize does not double it.
        assert_eq!(body, "\nbody\n");
        // llm-wiki house style: bare dates, inline lists.
        assert!(page_out.contains("created: 2026-10-25\n"));
        assert!(page_out.contains("tags: [technology]\n"));
    }

    #[test]
    fn wikilink_floor_counts_distinct_targets() {
        assert_eq!(count_outbound_wikilinks("see [[a]] and [[b]]"), 2);
        assert_eq!(count_outbound_wikilinks("[[a]] [[a]] [[a]]"), 1);
        assert_eq!(count_outbound_wikilinks("[[a|alias]] [[a]]"), 1);
        assert_eq!(count_outbound_wikilinks("[[a#h]] and [[a]]"), 1);
        assert_eq!(count_outbound_wikilinks("![[img.png]] [[a]]"), 1);
        assert_eq!(count_outbound_wikilinks("no links"), 0);
    }
}
