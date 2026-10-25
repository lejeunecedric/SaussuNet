//! index.md seed, following the llm-wiki skill "index.md Template".

use chrono::Utc;

/// Build the initial index.md (empty vault: `Total pages: 0`).
pub fn seed() -> String {
    let today = Utc::now().format("%Y-%m-%d");
    format!(
        "# Wiki Index

> Content catalog. Every wiki page listed under its type with a one-line summary.
> Read this first to find relevant pages for any query.
> Last updated: {today} | Total pages: 0

## Entities
<!-- Alphabetical within section -->

## Concepts

## Comparisons

## Queries
"
    )
}

#[cfg(test)]
mod tests {
    use super::seed;

    #[test]
    fn seed_has_header_and_sections() {
        let s = seed();
        assert!(s.contains("# Wiki Index"));
        assert!(s.contains("Total pages: 0"));
        for section in ["## Entities", "## Concepts", "## Comparisons", "## Queries"] {
            assert!(s.contains(section));
        }
    }
}
