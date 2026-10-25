//! Per-section `gen_hash` computation and three-way comparison (D-05).
//!
//! The writer records a map of section-slug → sha256 in every generated
//! page's frontmatter from the FIRST write (Pitfall 7 — the hash cannot be
//! retrofitted). On every later write the writer recomputes the map over the
//! on-disk file and compares it against the stored map: equal → agent-only
//! edits, safe overwrite; different → a human edited the page → conflict
//! (D-06).
//!
//! Self-hash rule (T-03-04): the `gen_hash` map must never hash itself. The
//! frontmatter hash is computed over the frontmatter YAML with the
//! `gen_hash` key REMOVED, canonically re-serialized (sorted keys, fixed
//! flow style) — identically on the write side (before the map is inserted)
//! and the check side (where the map is already present). Section hashes
//! cover the EXACT bytes of each section including its heading line, so
//! whitespace-only human edits are still detected.

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

/// Reserved key holding the frontmatter hash inside the gen_hash map.
pub const FRONTMATTER_KEY: &str = "__frontmatter__";

/// Frontmatter key the map lives under.
pub const GEN_HASH_KEY: &str = "gen_hash";

/// Split a page body into `## `-delimited sections.
///
/// A section starts at a line matching `^## ` (level-2 ATX heading); `###`
/// and deeper headings belong to the enclosing section. Text before the
/// first `## ` heading is the preamble section (slug `__preamble__`),
/// omitted when empty. Each section's text is the EXACT slice from its
/// heading line through the line before the next `## ` heading (trailing
/// newline included) so any byte-level human edit changes its hash.
///
/// Duplicate heading slugs get `-2`, `-3`, … suffixes so one section can
/// never mask another's hash.
pub fn split_sections(body: &str) -> Vec<(String, String)> {
    // Byte offsets of every line start.
    let mut starts: Vec<usize> = vec![0];
    for (i, b) in body.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i + 1);
        }
    }
    let is_section_head = |line: &str| line.starts_with("## ") && !line.starts_with("###");

    let mut sections: Vec<(String, String)> = Vec::new();
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut boundaries: Vec<usize> = Vec::new(); // start offset of each `## ` line

    for (idx, &start) in starts.iter().enumerate() {
        let end = starts.get(idx + 1).copied().unwrap_or(body.len());
        let line = body[start..end].trim_end_matches('\n');
        if is_section_head(line) {
            boundaries.push(start);
        }
    }

    // Preamble: everything before the first `## ` heading.
    let first = boundaries.first().copied().unwrap_or(body.len());
    if !body[..first].is_empty() {
        sections.push(("__preamble__".to_string(), body[..first].to_string()));
    }

    for (n, &start) in boundaries.iter().enumerate() {
        let end = boundaries.get(n + 1).copied().unwrap_or(body.len());
        let text = body[start..end].to_string();
        let heading = body[start..end]
            .lines()
            .next()
            .unwrap_or("")
            .trim_start_matches('#')
            .trim();
        let base = slugify(heading);
        let count = seen.entry(base.clone()).or_insert(0);
        *count += 1;
        let slug = if *count == 1 {
            base
        } else {
            format!("{base}-{count}")
        };
        sections.push((slug, text));
    }
    sections
}

/// kebab-case a heading: lowercase, runs of non-alphanumerics → single
/// hyphen, hyphens trimmed. `Gen Hash Rules!` → `gen-hash-rules`.
/// An empty result falls back to `section`.
fn slugify(heading: &str) -> String {
    let mut out = String::new();
    let mut prev_hyphen = false;
    for c in heading.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.extend(c.to_lowercase());
            prev_hyphen = false;
        } else if !prev_hyphen {
            out.push('-');
            prev_hyphen = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "section".to_string()
    } else {
        trimmed
    }
}

/// sha256 hex of each section's bytes (see [`split_sections`]).
///
/// Trailing newlines are trimmed before hashing: the blank line that
/// separates sections belongs to the preceding section's slice, so without
/// this an agent APPENDING a new section would change the previous
/// section's hash and falsely read as a human edit. Trailing SPACES on a
/// content line are kept — whitespace-only human edits stay detectable.
pub fn section_hashes(body: &str) -> BTreeMap<String, String> {
    split_sections(body)
        .into_iter()
        .map(|(slug, text)| {
            (
                slug,
                sha256_hex(text.trim_end_matches(['\n', '\r']).as_bytes()),
            )
        })
        .collect()
}

/// Hash the frontmatter YAML with the `gen_hash` key REMOVED (T-03-04).
///
/// Mandatory on BOTH sides: the write path hashes the new content's
/// frontmatter before the map is inserted; the check path reads the on-disk
/// frontmatter with the map present. Without stripping `gen_hash` the two
/// values could never match and every regeneration would falsely conflict.
/// Canonicalization (sorted keys, fixed flow style) makes the hash stable
/// against formatting-only differences in the YAML region.
pub fn frontmatter_hash(fm_yaml: &str) -> String {
    let value: serde_yml::Value = if fm_yaml.trim().is_empty() {
        serde_yml::Value::Null
    } else {
        serde_yml::from_str(fm_yaml).unwrap_or(serde_yml::Value::Null)
    };
    let mut out = String::new();
    canonical(&strip_gen_hash(&value), &mut out);
    sha256_hex(out.as_bytes())
}

/// Remove the `gen_hash` key from a parsed frontmatter mapping.
fn strip_gen_hash(value: &serde_yml::Value) -> serde_yml::Value {
    use serde_yml::Value;
    match value {
        Value::Mapping(map) => {
            let mut out = serde_yml::Mapping::new();
            for (k, v) in map {
                if k == GEN_HASH_KEY {
                    continue;
                }
                out.insert(k.clone(), v.clone());
            }
            Value::Mapping(out)
        }
        other => other.clone(),
    }
}

/// Canonical text form of a YAML value: mappings sort keys, sequences keep
/// order, scalars render deterministically (strings quoted to remove
/// type ambiguity). This is the byte string that gets hashed.
fn canonical(value: &serde_yml::Value, out: &mut String) {
    use serde_yml::Value;
    match value {
        Value::Null => out.push('~'),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => quote(s, out),
        Value::Sequence(seq) => {
            out.push('[');
            for (i, item) in seq.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canonical(item, out);
            }
            out.push(']');
        }
        Value::Mapping(map) => {
            let mut pairs: Vec<(String, String)> = map
                .iter()
                .map(|(k, v)| {
                    let mut ks = String::new();
                    quote(k, &mut ks);
                    let mut vs = String::new();
                    canonical(v, &mut vs);
                    (ks, vs)
                })
                .collect();
            pairs.sort();
            out.push('{');
            for (i, (k, v)) in pairs.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(k);
                out.push(':');
                out.push_str(v);
            }
            out.push('}');
        }
        other => out.push_str(&format!("{other:?}")),
    }
}

/// Deterministic quoted-string rendering (removes type ambiguity between
/// `"2026-10-25"` and a date-like scalar).
fn quote(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
}

/// The full gen_hash map for a page: per-section hashes plus the frontmatter
/// hash under `__frontmatter__`. Used IDENTICALLY on the write side (over
/// the new content, before the map is inserted) and the check side (over the
/// on-disk file) so the two maps are directly comparable.
pub fn compute_map(fm_yaml: &str, body: &str) -> BTreeMap<String, String> {
    let mut map = section_hashes(body);
    map.insert(FRONTMATTER_KEY.to_string(), frontmatter_hash(fm_yaml));
    map
}

/// True iff the on-disk file diverges from the map the writer recorded:
/// any key present in `stored` has a different value in `current` (changed
/// OR removed), or `current` carries a key absent from `stored` — a section
/// added to the file after the writer's last write (review W-3). The writer
/// re-embeds the FULL map on every write, so an unmapped on-disk key can
/// only be a human addition; D-06 forbids silently deleting it, so it routes
/// to conflict exactly like a changed section. (Sections the agent adds to
/// the INCOMING content are unaffected: `current` is computed over the
/// on-disk file, not the incoming page.)
pub fn human_edited(stored: &BTreeMap<String, String>, current: &BTreeMap<String, String>) -> bool {
    stored
        .iter()
        .any(|(k, v)| current.get(k).map(|c| c != v).unwrap_or(true))
        || current.keys().any(|k| !stored.contains_key(k))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_on_level2_headings_only() {
        let body = "intro\n\n## One\nbody one\n\n### Nested\nstill one\n\n## Two\nbody two\n";
        let sections = split_sections(body);
        let slugs: Vec<&str> = sections.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(slugs, vec!["__preamble__", "one", "two"]);
        // `###` belongs to the enclosing section.
        let one = &sections[1].1;
        assert!(one.starts_with("## One\n"));
        assert!(one.contains("### Nested"));
        assert!(!one.contains("## Two"));
    }

    #[test]
    fn no_preamble_section_when_body_starts_with_heading() {
        let sections = split_sections("## One\nx\n");
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].0, "one");
    }

    #[test]
    fn slug_kebab_cases_heading() {
        let body = "## Gen Hash Rules!\ncontent\n";
        let sections = split_sections(body);
        assert_eq!(sections[0].0, "gen-hash-rules");
    }

    #[test]
    fn duplicate_headings_get_distinct_slugs() {
        let body = "## Dup\na\n\n## Dup\nb\n";
        let sections = split_sections(body);
        let slugs: Vec<&str> = sections.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(slugs, vec!["dup", "dup-2"]);
        assert_ne!(sections[0].1, sections[1].1);
    }

    #[test]
    fn whitespace_only_edit_changes_section_hash() {
        let a = section_hashes("## One\nbody\n");
        let b = section_hashes("## One\nbody \n"); // trailing space only
        assert_ne!(a["one"], b["one"]);
    }

    #[test]
    fn human_edited_flags_changed_removed_and_added() {
        let stored = compute_map("title: X\n", "## One\na\n\n## Two\nb\n");
        // Review W-3: a section present in the file but absent from the
        // stored map is a human ADDITION (the writer re-embeds the full map
        // on every write, so an unmapped key can only be human) → flagged.
        let added = compute_map("title: X\n", "## One\na\n\n## Two\nb\n\n## My Notes\nc\n");
        assert!(human_edited(&stored, &added));
        // Human changes a section.
        let changed = compute_map("title: X\n", "## One\nEDITED\n\n## Two\nb\n");
        assert!(human_edited(&stored, &changed));
        // Human removes a section.
        let removed = compute_map("title: X\n", "## One\na\n");
        assert!(human_edited(&stored, &removed));
        // Identical content: no edit.
        assert!(!human_edited(
            &stored,
            &compute_map("title: X\n", "## One\na\n\n## Two\nb\n")
        ));
    }

    #[test]
    fn frontmatter_only_edit_detected_via_frontmatter_key() {
        let body = "## One\na\n";
        let before = compute_map("title: X\n", body);
        let after = compute_map("title: X\nconfidence: high\n", body);
        assert!(human_edited(&before, &after));
        // Section hashes themselves are untouched — only __frontmatter__ moved.
        assert_eq!(before["one"], after["one"]);
        assert_ne!(before[FRONTMATTER_KEY], after[FRONTMATTER_KEY]);
    }

    #[test]
    fn gen_hash_key_never_hashes_itself() {
        // The self-hash guard at the ghash level: a map computed over yaml
        // equals the map recomputed over the same frontmatter WITH the map
        // already embedded (what the check side reads from disk).
        let yaml = "title: X\ntags: [technology]\n";
        let body = "## One\na\n";
        let map = compute_map(yaml, body);
        let mut embedded = String::from(yaml);
        embedded.push_str("gen_hash:\n");
        for (k, v) in &map {
            embedded.push_str(&format!("  {k}: {v}\n"));
        }
        assert_eq!(map, compute_map(&embedded, body));
    }

    #[test]
    fn canonicalization_is_key_order_stable() {
        let a = frontmatter_hash("title: X\ntags: [a, b]\n");
        let b = frontmatter_hash("tags: [a, b]\ntitle: X\n");
        assert_eq!(a, b);
    }
}
