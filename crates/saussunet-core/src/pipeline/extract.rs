//! HTML → clean article Markdown (D-05: `readability` + `htmd`, pure Rust).
//!
//! `readability` finds the article body + title; `htmd` converts HTML to
//! Markdown. When readability finds no article (thin pages, docs sites) we
//! fall back to converting the WHOLE document — a job must never fail just
//! because a page has no `<article>`-shaped content.

use url::Url;

use crate::error::{CoreError, Result};

/// Marker base URL for the reader (readability 0.3 requires a URL for
/// link resolution; the fetched page's own URL is not needed for body
/// scoring here).
const READER_BASE: &str = "https://saussunet.invalid/";

/// Extract `(title, markdown)` from an HTML document. Title may be empty
/// when the fallback path is used and no title can be recovered.
pub fn html_to_markdown(html: &str) -> Result<(String, String)> {
    let base = Url::parse(READER_BASE).map_err(|e| CoreError::Fetch {
        url: String::new(),
        reason: format!("reader base url: {e}"),
    })?;
    let article = readability::extractor::extract(&mut html.as_bytes(), &base).ok();
    match article {
        Some(product) => {
            let markdown = htmd::convert(&product.content).map_err(|e| CoreError::Fetch {
                url: String::new(),
                reason: format!("markdown conversion failed: {e}"),
            })?;
            Ok((product.title, markdown))
        }
        None => {
            let markdown = htmd::convert(html).map_err(|e| CoreError::Fetch {
                url: String::new(),
                reason: format!("markdown conversion failed: {e}"),
            })?;
            Ok((String::new(), markdown))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_article_body() {
        let html = r#"<html><head><title>My Post</title></head><body>
            <nav>menu junk</nav>
            <article><h1>My Post</h1><p>The actual content of the article is
            long enough for readability to consider it the main content of
            this page, with several sentences and real substance behind it.</p>
            <p>A second paragraph with even more words so the scorer picks
            this node as the article body of the document.</p></article>
            <footer>ads</footer></body></html>"#;
        let (title, md) = html_to_markdown(html).expect("extract");
        assert!(md.contains("actual content"), "markdown: {md}");
        assert!(!md.contains("menu junk"), "nav junk must be dropped: {md}");
        let _ = title;
    }

    #[test]
    fn thin_page_falls_back_to_whole_document() {
        let html = "<html><body><p>tiny</p></body></html>";
        let (_title, md) = html_to_markdown(html).expect("fallback must not fail");
        assert!(md.contains("tiny"));
    }
}
