//! HTML → clean article Markdown (D-05: `readability` + `htmd`, pure Rust).
//!
//! `readability` finds the article body + title; `htmd` converts HTML to
//! Markdown. When readability finds no article (thin pages, docs sites) we
//! fall back to converting the WHOLE document — a job must never fail just
//! because a page has no `<article>`-shaped content.

use std::path::Path;
use std::time::Duration;

use url::Url;

use crate::error::{CoreError, Result};

/// Hard bound on a single pdftotext run (threat model: a hostile PDF must
/// never pin a worker forever).
const PDFTOTEXT_TIMEOUT: Duration = Duration::from_secs(60);

/// Extract text from a PDF via the `pdftotext` subprocess (D-06: poppler,
/// LOUD failure if missing, NO pure-Rust fallback). Fixed flags
/// (`-q -layout <pdf> -`) + the guarded path only — no shell, no user string
/// interpolated into args. A missing binary surfaces as
/// [`CoreError::MissingBinary`] with the actionable install hint (the
/// Display string lands in jobs.error on purpose).
///
/// Binary path: env `SAUSSUNET_PDFTOTEXT_PATH` overrides the default
/// `pdftotext` (documented injection point for the missing-binary test).
pub async fn pdf_to_text(pdf: &Path) -> Result<String> {
    let binary =
        std::env::var("SAUSSUNET_PDFTOTEXT_PATH").unwrap_or_else(|_| "pdftotext".to_string());
    let spawned = tokio::process::Command::new(&binary)
        .arg("-q")
        .arg("-layout")
        .arg(pdf)
        .arg("-")
        .output();
    let out = match tokio::time::timeout(PDFTOTEXT_TIMEOUT, spawned).await {
        Err(_elapsed) => {
            return Err(CoreError::Fetch {
                url: pdf.display().to_string(),
                reason: format!("pdftotext timed out after {}s", PDFTOTEXT_TIMEOUT.as_secs()),
            })
        }
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(CoreError::MissingBinary {
                binary,
                hint: "install poppler-utils (apt install poppler-utils)".to_string(),
            })
        }
        Ok(Err(e)) => {
            return Err(CoreError::Fetch {
                url: pdf.display().to_string(),
                reason: format!("pdftotext spawn failed: {e}"),
            })
        }
        Ok(Ok(out)) => out,
    };
    if !out.status.success() {
        // Truncated stderr tail only — never the PDF bytes or full text (D-03).
        let stderr = String::from_utf8_lossy(&out.stderr);
        let tail: String = stderr
            .chars()
            .rev()
            .take(400)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        return Err(CoreError::Fetch {
            url: pdf.display().to_string(),
            reason: format!("pdftotext exited with {}: {}", out.status, tail),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

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
