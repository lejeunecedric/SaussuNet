//! log.md seed, following the llm-wiki skill "log.md Template" (append-only,
//! 500-entry rotation rule stated in the header blockquote).

use chrono::Utc;

/// Build the initial log.md with the creation entry for domain `domain`.
pub fn seed(domain: &str) -> String {
    let today = Utc::now().format("%Y-%m-%d");
    format!(
        "# Wiki Log

> Chronological record of all wiki actions. Append-only.
> Format: `## [YYYY-MM-DD] action | subject`
> Actions: ingest, update, query, lint, create, archive, delete
> When this file exceeds 500 entries, rotate: rename to log-YYYY.md, start fresh.

## [{today}] create | Wiki initialized
- Domain: {domain}
- Structure created with SCHEMA.md, index.md, log.md
"
    )
}

#[cfg(test)]
mod tests {
    use super::seed;

    #[test]
    fn seed_has_header_and_first_entry() {
        let s = seed("main");
        assert!(s.contains("# Wiki Log"));
        assert!(s.contains("## ["));
        assert!(s.contains("Wiki initialized"));
        assert!(s.contains("- Domain: main"));
        assert!(s.contains("rotate"));
    }
}
