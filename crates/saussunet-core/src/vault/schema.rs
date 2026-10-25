//! SCHEMA.md seed, following the llm-wiki skill template (format authority:
//! `/home/c/.hermes/profiles/devo/skills/research/llm-wiki/SKILL.md`,
//! "SCHEMA.md Template") plus the two SaussuNet writer rules (D-05/D-07).

use crate::error::{CoreError, Result};

use super::Vault;

/// Parse the tag taxonomy from the vault's SCHEMA.md: bullet lines under
/// `## Tag Taxonomy` shaped `- Category: tag1, tag2, ...` (the seeded
/// format). Returns the flat list of allowed tags.
pub fn parse_tag_taxonomy(vault: &Vault) -> Result<Vec<String>> {
    let path = vault.root().join("SCHEMA.md");
    let content = std::fs::read_to_string(&path).map_err(|e| CoreError::Io {
        path: path.clone(),
        source: e,
    })?;
    let mut tags = Vec::new();
    let mut in_section = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("## ") {
            in_section = trimmed == "## Tag Taxonomy";
            continue;
        }
        if !in_section {
            continue;
        }
        if let Some(bullet) = trimmed.strip_prefix("- ") {
            // `- People/Orgs: person, company, ...` — tags follow the colon.
            if let Some((_, list)) = bullet.split_once(':') {
                tags.extend(
                    list.split(',')
                        .map(|t| t.trim().to_string())
                        .filter(|t| !t.is_empty()),
                );
            }
        }
    }
    if tags.is_empty() {
        // Loud failure (§2.8): a vault whose SCHEMA.md lost its taxonomy
        // would silently accept any tag — refuse instead.
        return Err(CoreError::Vault(format!(
            "no tag taxonomy found under '## Tag Taxonomy' in {} — fix SCHEMA.md",
            path.display()
        )));
    }
    Ok(tags)
}

/// Build the SCHEMA.md seed for a vault whose Domain is `domain` (the vault
/// name from config).
pub fn seed(domain: &str) -> String {
    format!(
        r#"# Wiki Schema

## Domain
{domain}

## Conventions
- File names: lowercase, hyphens, no spaces (e.g., `transformer-architecture.md`)
- Every wiki page starts with YAML frontmatter (see below)
- Use `[[wikilinks]]` to link between pages (minimum 2 outbound links per page)
- When updating a page, always bump the `updated` date
- Every new page must be added to `index.md` under the correct section
- Every action must be appended to `log.md`
- **Provenance markers:** On pages that synthesize 3+ sources, append `^[raw/articles/source-file.md]`
  at the end of paragraphs whose claims come from a specific source. This lets a reader trace each
  claim back without re-reading the whole raw file. Optional on single-source pages where the
  `sources:` frontmatter is enough.
- The `gen_hash` frontmatter map is writer-managed (section slug → hash): do not hand-edit it.
  A page in `entities/`/`concepts/` with no `gen_hash` map is human-owned and the writer refuses
  to touch it (D-05/D-08).
- `protected: true` in frontmatter marks user-adopted pages: the writer refuses ALL writes to
  them; proposals against them are recorded as conflicts instead (D-07).

## Frontmatter
```yaml
---
title: Page Title
created: YYYY-MM-DD
updated: YYYY-MM-DD
type: entity | concept | comparison | query | summary
tags: [from Tag Taxonomy below]
sources: [raw/articles/source-name.md]
# Optional quality signals:
confidence: high | medium | low        # how well-supported the claims are
contested: true                        # set when the page has unresolved contradictions
contradictions: [other-page-slug]      # pages this one conflicts with
---
```

`confidence` and `contested` are optional but recommended for opinion-heavy or fast-moving
topics. Lint surfaces `contested: true` and `confidence: low` pages for review so weak claims
don't silently harden into accepted wiki fact.

### raw/ Frontmatter

Raw sources ALSO get a small frontmatter block so re-ingests can detect drift:

```yaml
---
source_url: https://example.com/article   # original URL, if applicable
ingested: YYYY-MM-DD
sha256: <hex digest of the raw content below the frontmatter>
---
```

The `sha256:` lets a future re-ingest of the same URL skip processing when content is unchanged,
and flag drift when it has changed. Compute over the body only (everything after the closing
`---`), not the frontmatter itself.

## Tag Taxonomy
Add new tags here BEFORE using them. Every tag on a page must appear in this taxonomy —
this rule prevents tag sprawl.

- People/Orgs: person, company, organization, place
- Topics: technology, science, health, finance, history, product, event
- Meta: comparison, timeline, reference, process

## Page Thresholds
- **Create a page** when an entity/concept appears in 2+ sources OR is central to one source
- **Add to existing page** when a source mentions something already covered
- **DON'T create a page** for passing mentions, minor details, or things outside the domain
- **Split a page** when it exceeds ~200 lines — break into sub-topics with cross-links
- **Archive a page** when its content is fully superseded — move to `_archive/`, remove from index

## Entity Pages
One page per notable entity. Include:
- Overview / what it is
- Key facts and dates
- Relationships to other entities ([[wikilinks]])
- Source references

## Concept Pages
One page per concept or topic. Include:
- Definition / explanation
- Current state of knowledge
- Open questions or debates
- Related concepts ([[wikilinks]])

## Comparison Pages
Side-by-side analyses. Include:
- What is being compared and why
- Dimensions of comparison (table format preferred)
- Verdict or synthesis
- Sources

## Update Policy
When new information conflicts with existing content:
1. Check the dates — newer sources generally supersede older ones
2. If genuinely contradictory, note both positions with dates and sources
3. Mark the contradiction in frontmatter: `contradictions: [page-name]`
4. Flag for user review in the lint report
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_contains_required_sections() {
        let s = seed("main");
        assert!(s.contains("# Wiki Schema"));
        assert!(s.contains("## Domain\nmain"));
        assert!(s.contains("## Tag Taxonomy"));
        for tag in ["person", "technology", "process"] {
            assert!(s.contains(tag), "seed must contain tag {tag}");
        }
        assert!(s.contains("gen_hash"));
        assert!(s.contains("protected: true"));
    }

    #[tokio::test]
    async fn parse_tag_taxonomy_reads_seeded_schema() {
        let tmp = tempfile::TempDir::new().unwrap();
        let vault = Vault::new(1, "main", tmp.path().to_path_buf());
        vault.init_fs().await.unwrap();
        let tags = parse_tag_taxonomy(&vault).unwrap();
        for t in ["person", "company", "technology", "process", "comparison"] {
            assert!(tags.iter().any(|x| x == t), "missing {t} in {tags:?}");
        }
    }

    #[tokio::test]
    async fn parse_tag_taxonomy_fails_loudly_when_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        super::super::atomic::write_atomic(
            &tmp.path().join("SCHEMA.md"),
            b"# Wiki Schema\n\nno taxonomy here\n",
        )
        .await
        .unwrap();
        let vault = Vault::new(1, "main", tmp.path().to_path_buf());
        let err = parse_tag_taxonomy(&vault).unwrap_err().to_string();
        assert!(err.contains("no tag taxonomy"), "{err}");
    }
}
