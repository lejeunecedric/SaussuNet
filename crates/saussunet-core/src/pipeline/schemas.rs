//! Typed pass schemas for the two-pass extraction (D-09, plan 03-03).
//!
//! Tolerant-parse friendly per Phase 2 D-05: EVERY field is
//! `#[serde(default)]`, unknown fields are ignored, and there is deliberately
//! NO `deny_unknown_fields` — a chatty model still yields usable output.

use serde::Deserialize;

/// Pass-1 candidate kind. Unknown strings fold to `Concept` (tolerant
/// fallback via `#[serde(other)]`) — a mislabelled kind must never kill a
/// whole extraction pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CandidateKind {
    Entity,
    #[default]
    #[serde(other)]
    Concept,
}

/// One pass-1 candidate (D-09/D-10/D-16): name, kind, mention count, the
/// LLM's centrality judgment + one-line justification, and alias variants
/// found in the source.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Candidate {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub kind: CandidateKind,
    #[serde(default)]
    pub mentions: u32,
    #[serde(default)]
    pub central: bool,
    #[serde(default)]
    pub justification: String,
    #[serde(default)]
    pub aliases: Vec<String>,
}

/// Pass-1 output envelope.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ExtractCandidates {
    #[serde(default)]
    pub candidates: Vec<Candidate>,
}

impl ExtractCandidates {
    /// Small example-object embedded in the pass-1 prompt (the tolerant
    /// parser does the real validation work).
    pub fn json_schema_hint() -> serde_json::Value {
        serde_json::json!({
            "candidates": [{
                "name": "React",
                "kind": "entity",
                "mentions": 7,
                "central": true,
                "justification": "the article is a deep-dive on React's new compiler",
                "aliases": ["ReactJS", "react"]
            }]
        })
    }
}

/// One pass-2 page section (heading + markdown body carrying `^[raw/...]`
/// provenance markers per claim, D-11).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Section {
    #[serde(default)]
    pub heading: String,
    #[serde(default)]
    pub markdown: String,
}

/// Pass-2 output: full page content for one threshold-passing page
/// (D-09/D-11/D-12). `contested` + `contradictions` implement the
/// never-auto-resolve contradiction rule.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PageContent {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub page_type: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub sections: Vec<Section>,
    #[serde(default)]
    pub contested: bool,
    #[serde(default)]
    pub contradictions: Vec<String>,
}

impl PageContent {
    /// Small example-object embedded in the pass-2 prompt.
    pub fn json_schema_hint() -> serde_json::Value {
        serde_json::json!({
            "title": "React",
            "page_type": "entity",
            "tags": ["technology"],
            "summary": "React is a JavaScript UI library ... ^[raw/2026-10-30-post.md]",
            "sections": [{
                "heading": "Overview",
                "markdown": "Maintained by Meta ^[raw/2026-10-30-post.md]. See [[Meta]]."
            }],
            "contested": false,
            "contradictions": []
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_candidates_parses_realistic_json() {
        let raw = r#"{
            "candidates": [
                {"name": "React", "kind": "entity", "mentions": 7, "central": true,
                 "justification": "deep-dive", "aliases": ["ReactJS"]},
                {"name": "Server Components", "kind": "concept", "mentions": 1,
                 "central": false, "justification": "passing mention", "aliases": []}
            ],
            "note": "unknown key must be ignored"
        }"#;
        let parsed: ExtractCandidates = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.candidates.len(), 2);
        let first = &parsed.candidates[0];
        assert_eq!(first.name, "React");
        assert_eq!(first.kind, CandidateKind::Entity);
        assert_eq!(first.mentions, 7);
        assert!(first.central);
        assert_eq!(first.aliases, vec!["ReactJS".to_string()]);
        assert_eq!(parsed.candidates[1].kind, CandidateKind::Concept);
    }

    #[test]
    fn missing_fields_and_unknown_kind_fold_to_defaults() {
        let parsed: ExtractCandidates = serde_json::from_str(
            r#"{"candidates": [{"name": "X", "kind": "widget", "extra": 1}], "junk": true}"#,
        )
        .unwrap();
        let c = &parsed.candidates[0];
        assert_eq!(c.name, "X");
        assert_eq!(
            c.kind,
            CandidateKind::Concept,
            "unknown kind → tolerant fallback"
        );
        assert_eq!(c.mentions, 0);
        assert!(!c.central);
        assert!(c.aliases.is_empty());
        // Completely empty envelope still parses.
        let empty: ExtractCandidates = serde_json::from_str("{}").unwrap();
        assert!(empty.candidates.is_empty());
    }

    #[test]
    fn page_content_parses_with_contested_and_tolerates_gaps() {
        let parsed: PageContent = serde_json::from_str(
            r#"{"title": "React", "page_type": "entity", "tags": ["technology"],
                "summary": "s ^[raw/x.md]",
                "sections": [{"heading": "Overview", "markdown": "m ^[raw/x.md] [[Meta]]",
                               "unknown": 2}],
                "contested": true, "contradictions": ["vue"]}"#,
        )
        .unwrap();
        assert!(parsed.contested);
        assert_eq!(parsed.contradictions, vec!["vue".to_string()]);
        assert_eq!(parsed.sections[0].heading, "Overview");
        // Missing everything → all defaults.
        let blank: PageContent = serde_json::from_str("{}").unwrap();
        assert!(blank.title.is_empty() && !blank.contested && blank.sections.is_empty());
    }

    #[test]
    fn schema_hints_are_objects() {
        assert!(ExtractCandidates::json_schema_hint()
            .get("candidates")
            .is_some());
        assert!(PageContent::json_schema_hint().get("sections").is_some());
    }
}
