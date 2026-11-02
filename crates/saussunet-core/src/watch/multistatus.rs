//! RFC 4918 multistatus / RFC 6578 sync-report parser (pure, no HTTP).
//!
//! Namespace-aware DOM via roxmltree: no DTD fetching, undefined entities
//! error out — billion-laughs expansion is inert (threat model, 04-03).
//! Hrefs are percent-decoded ONCE here; everything downstream (state keys,
//! diffs) lives in decoded space; request URLs are rebuilt with `Url::join`
//! at the edge (04-04).

use roxmltree::{Document, Node};

use crate::error::{CoreError, Result};

/// One `<d:response>` item: the per-file facts a Depth:1 walk diffs against
/// `watch_state`. `path` is decoded, root-relative, '/' separators, no
/// leading/trailing '/'.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DavEntry {
    pub path: String,
    /// Verbatim, including the weak `W/"…"` prefix (D-07: sha256 backstop
    /// lives in the diff/poller, never here).
    pub etag: Option<String>,
    /// `getcontentlength` (absent on collections).
    pub size: Option<i64>,
    /// resourcetype `<collection/>` OR trailing-slash href.
    pub is_collection: bool,
    /// Per-item `<d:status>` code; 200 when absent (plain PROPFIND).
    /// Sync reports: 200 = added/changed, 404 = removed.
    pub status: u16,
}

/// Parse a 207 multistatus body into `DavEntry` items.
///
/// `root_path` is the walked collection's percent-ENCODED URI path (taken
/// from the request `Url`); collection-self (href == root) is SKIPPED because
/// Depth:1 always includes self.
pub fn parse_multistatus(xml: &str, root_path: &str) -> Result<Vec<DavEntry>> {
    // roxmltree: read-only DOM, no DTD fetching, errors on undefined entities.
    let doc = Document::parse(xml).map_err(|e| CoreError::Dav(format!("xml: {e}")))?;
    let decoded_root = percent_decode(root_path.as_bytes());
    let decoded_root = decoded_root.trim_matches('/');

    let mut out = Vec::new();
    for resp in doc.root().descendants().filter(|n| is_dav(*n, "response")) {
        let Some(href_raw) = resp
            .children()
            .find(|n| is_dav(*n, "href"))
            .and_then(text_of)
        else {
            continue; // response without href: nothing to key on
        };
        let href_decoded = percent_decode(href_raw.trim().as_bytes());
        let path = relativize(&href_decoded, decoded_root);
        if path.is_empty() {
            continue; // collection-self — Depth:1 includes it, diff doesn't want it
        }

        // Props count only under a propstat whose status line is 200.
        let mut etag = None;
        let mut size = None;
        let mut is_collection = false;
        for ps in resp.children().filter(|n| is_dav(*n, "propstat")) {
            if !propstat_is_ok(ps) {
                continue;
            }
            let Some(prop) = ps.children().find(|n| is_dav(*n, "prop")) else {
                continue;
            };
            for child in prop.children() {
                if is_dav(child, "getetag") {
                    etag = text_of(child).map(|t| t.trim().to_string());
                } else if is_dav(child, "getcontentlength") {
                    size = text_of(child).and_then(|t| t.trim().parse::<i64>().ok());
                } else if is_dav(child, "resourcetype")
                    && child
                        .children()
                        .any(|c| is_dav(c, "collection") || c.has_tag_name("collection"))
                {
                    is_collection = true;
                }
            }
        }
        // Trailing-slash href is the fallback collection signal.
        if href_decoded.trim_end().ends_with('/') {
            is_collection = true;
        }

        // Per-item <d:status> (direct child, NOT the propstat ones): sync-report
        // 404 marks a removal. Absent (plain PROPFIND) → 200.
        let status = resp
            .children()
            .find(|n| is_dav(*n, "status"))
            .and_then(text_of)
            .and_then(|t| parse_status_code(t.trim()))
            .unwrap_or(200);

        out.push(DavEntry {
            path,
            etag,
            size,
            is_collection,
            status,
        });
    }
    Ok(out)
}

/// The trailing `<d:sync-token>` text (RFC 6578), trimmed. `None` for plain
/// PROPFIND bodies. Persisted via `Store::set_sync_token` (04-04/04-07).
pub fn extract_sync_token(xml: &str) -> Option<String> {
    let doc = Document::parse(xml).ok()?;
    doc.root()
        .descendants()
        .rfind(|n| is_dav(*n, "sync-token"))
        .and_then(text_of)
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// `true` iff `node` is in the DAV: namespace with this local name.
fn is_dav(node: Node, tag: &str) -> bool {
    node.has_tag_name(tag) && node.tag_name().namespace() == Some("DAV:")
}

/// Node's concatenated text, if it has any.
fn text_of(node: Node) -> Option<String> {
    node.text().map(|t| t.to_string())
}

/// A propstat's props are trusted only when its `<status>` line contains
/// `" 200 "` (e.g. `HTTP/1.1 200 OK`).
fn propstat_is_ok(propstat: Node) -> bool {
    propstat
        .children()
        .find(|n| is_dav(*n, "status"))
        .and_then(text_of)
        .is_some_and(|t| t.contains(" 200 "))
}

/// Extract the numeric code from an HTTP status line (`HTTP/1.1 404 Not Found`).
fn parse_status_code(line: &str) -> Option<u16> {
    line.split_whitespace()
        .nth(1)
        .and_then(|c| c.parse::<u16>().ok())
}

/// Percent-decode raw bytes (UTF-8 byte-level; '+' stays literal — WebDAV
/// hrefs use %20, never '+'). Invalid `%` sequences pass through verbatim.
fn percent_decode(input: &[u8]) -> String {
    let mut out: Vec<u8> = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        if input[i] == b'%' && i + 2 < input.len() {
            let hi = (input[i + 1] as char).to_digit(16);
            let lo = (input[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(input[i]);
        i += 1;
    }
    // Input came from a &str; decoded bytes stay valid UTF-8 for well-formed
    // hrefs. Lossy as the defensive floor for hostile input.
    String::from_utf8(out).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

/// Turn a decoded href into the root-relative key: strip the decoded root
/// prefix, trim slashes. Non-matching hrefs (crafted or server-buggy) keep
/// their full path minus the leading '/' — an opaque relative key, never an
/// absolute path (threat model: path traversal).
fn relativize(decoded_href: &str, decoded_root: &str) -> String {
    // Compare in slash-trimmed space: root arrives "/a/b/", hrefs "/a/b/c".
    let h = decoded_href.trim().trim_matches('/');
    if decoded_root.is_empty() {
        return h.to_string();
    }
    let prefix = format!("{decoded_root}/");
    let stripped = match h.strip_prefix(prefix.as_str()) {
        Some(rest) => rest,
        None if h == decoded_root => "",
        None => h,
    };
    stripped.trim_matches('/').to_string()
}

#[cfg(test)]
mod tests {
    use super::{extract_sync_token, parse_multistatus, DavEntry};

    const ROOT: &str = "/dav/vault-inbox/";

    /// Realistic Depth:1 207: self first, note.md, papers/ collection,
    /// percent-encoded hrefs, and a file whose getetag comes back under a
    /// 404 propstat (must be ignored).
    const PROP207: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:multistatus xmlns:d="DAV:">
  <d:response>
    <d:href>/dav/vault-inbox/</d:href>
    <d:propstat>
      <d:prop>
        <d:resourcetype><d:collection/></d:resourcetype>
        <d:getetag>"root-etag"</d:getetag>
      </d:prop>
      <d:status>HTTP/1.1 200 OK</d:status>
    </d:propstat>
  </d:response>
  <d:response>
    <d:href>/dav/vault-inbox/note.md</d:href>
    <d:propstat>
      <d:prop>
        <d:resourcetype/>
        <d:getetag>"abc123"</d:getetag>
        <d:getcontentlength>42</d:getcontentlength>
      </d:prop>
      <d:status>HTTP/1.1 200 OK</d:status>
    </d:propstat>
  </d:response>
  <d:response>
    <d:href>/dav/vault-inbox/papers/</d:href>
    <d:propstat>
      <d:prop>
        <d:resourcetype><d:collection/></d:resourcetype>
      </d:prop>
      <d:status>HTTP/1.1 200 OK</d:status>
    </d:propstat>
  </d:response>
  <d:response>
    <d:href>/dav/vault-inbox/a%20b.md</d:href>
    <d:propstat>
      <d:prop>
        <d:resourcetype/>
        <d:getetag>"sp"</d:getetag>
        <d:getcontentlength>3</d:getcontentlength>
      </d:prop>
      <d:status>HTTP/1.1 200 OK</d:status>
    </d:propstat>
  </d:response>
  <d:response>
    <d:href>/dav/vault-inbox/caf%C3%A9.txt</d:href>
    <d:propstat>
      <d:prop>
        <d:resourcetype/>
        <d:getetag>"cf"</d:getetag>
        <d:getcontentlength>5</d:getcontentlength>
      </d:prop>
      <d:status>HTTP/1.1 200 OK</d:status>
    </d:propstat>
  </d:response>
  <d:response>
    <d:href>/dav/vault-inbox/broken.bin</d:href>
    <d:propstat>
      <d:prop><d:getetag>"never-trusted"</d:getetag></d:prop>
      <d:status>HTTP/1.1 404 Not Found</d:status>
    </d:propstat>
    <d:propstat>
      <d:prop><d:resourcetype/></d:prop>
      <d:status>HTTP/1.1 200 OK</d:status>
    </d:propstat>
  </d:response>
</d:multistatus>"#;

    /// RFC 6578 sync-report 207: one 200-item (added/changed), one 404-item
    /// (removed), trailing sync-token.
    const SYNC207: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:multistatus xmlns:d="DAV:">
  <d:response>
    <d:href>/dav/vault-inbox/new.md</d:href>
    <d:propstat>
      <d:prop>
        <d:getetag>"n1"</d:getetag>
        <d:getcontentlength>7</d:getcontentlength>
      </d:prop>
      <d:status>HTTP/1.1 200 OK</d:status>
    </d:propstat>
    <d:status>HTTP/1.1 200 OK</d:status>
  </d:response>
  <d:response>
    <d:href>/dav/vault-inbox/gone.md</d:href>
    <d:propstat>
      <d:prop/>
      <d:status>HTTP/1.1 404 Not Found</d:status>
    </d:propstat>
    <d:status>HTTP/1.1 404 Not Found</d:status>
  </d:response>
  <d:sync-token>http://example.com/ns/sync/42</d:sync-token>
</d:multistatus>"#;

    fn find<'a>(entries: &'a [DavEntry], path: &str) -> &'a DavEntry {
        entries
            .iter()
            .find(|e| e.path == path)
            .unwrap_or_else(|| panic!("no entry for {path:?}, got: {:?}", entries))
    }

    #[test]
    fn parses_plain_propfind() {
        let entries = parse_multistatus(PROP207, ROOT).expect("parse");
        // Collection-self (href == root) is skipped — Depth:1 includes self.
        assert!(
            !entries.iter().any(|e| e.path.is_empty()),
            "collection-self must be skipped: {entries:?}"
        );
        assert_eq!(entries.len(), 5, "got: {entries:?}");

        let note = find(&entries, "note.md");
        assert_eq!(note.etag.as_deref(), Some("\"abc123\""));
        assert_eq!(note.size, Some(42));
        assert!(!note.is_collection);
        assert_eq!(note.status, 200);

        let papers = find(&entries, "papers");
        assert!(papers.is_collection, "resourcetype <collection/>");
        assert_eq!(papers.size, None, "collections carry no getcontentlength");
    }

    #[test]
    fn decodes_percent_hrefs_and_relativizes() {
        let entries = parse_multistatus(PROP207, ROOT).expect("parse");
        assert!(find(&entries, "a b.md").path == "a b.md");
        assert!(find(&entries, "café.txt").path == "café.txt");
        // No leading '/', no trailing '/' on any parsed path.
        for e in &entries {
            assert!(!e.path.starts_with('/'), "{}", e.path);
            assert!(!e.path.ends_with('/'), "{}", e.path);
        }
    }

    #[test]
    fn ignores_props_under_404_propstat() {
        let entries = parse_multistatus(PROP207, ROOT).expect("parse");
        let broken = find(&entries, "broken.bin");
        assert_eq!(
            broken.etag, None,
            "getetag under a 404 propstat must not be trusted"
        );
        assert!(!broken.is_collection);
    }

    #[test]
    fn weak_etag_preserved_verbatim() {
        let xml = r#"<d:multistatus xmlns:d="DAV:"><d:response>
<d:href>/dav/vault-inbox/w.md</d:href>
<d:propstat><d:prop><d:resourcetype/><d:getetag>W/"abc"</d:getetag></d:prop>
<d:status>HTTP/1.1 200 OK</d:status></d:propstat>
</d:response></d:multistatus>"#;
        let entries = parse_multistatus(xml, ROOT).expect("parse");
        assert_eq!(find(&entries, "w.md").etag.as_deref(), Some("W/\"abc\""));
    }

    #[test]
    fn sync_report_marks_removed_items() {
        let entries = parse_multistatus(SYNC207, ROOT).expect("parse");
        assert_eq!(entries.len(), 2);
        let nw = find(&entries, "new.md");
        assert_eq!(nw.status, 200);
        assert_eq!(nw.etag.as_deref(), Some("\"n1\""));
        assert_eq!(nw.size, Some(7));
        let gone = find(&entries, "gone.md");
        assert_eq!(gone.status, 404, "per-item <d:status> 404 = removed");
    }

    #[test]
    fn extracts_sync_token() {
        assert_eq!(
            extract_sync_token(SYNC207).as_deref(),
            Some("http://example.com/ns/sync/42")
        );
        // Plain PROPFIND has no sync-token.
        assert_eq!(extract_sync_token(PROP207), None);
    }

    #[test]
    fn trailing_slash_fallback_collection_detection() {
        // No resourcetype element at all — trailing-slash href is the fallback.
        let xml = r#"<d:multistatus xmlns:d="DAV:"><d:response>
<d:href>/dav/vault-inbox/sub/</d:href>
<d:propstat><d:prop><d:getetag>"s"</d:getetag></d:prop>
<d:status>HTTP/1.1 200 OK</d:status></d:propstat>
</d:response></d:multistatus>"#;
        let entries = parse_multistatus(xml, ROOT).expect("parse");
        assert!(find(&entries, "sub").is_collection);
    }

    #[test]
    fn traversal_href_stays_relative() {
        // Threat model: a crafted href is an opaque DB/log key only — it must
        // never parse into an absolute path. This plan writes nothing to disk.
        let xml = r#"<d:multistatus xmlns:d="DAV:"><d:response>
<d:href>/dav/vault-inbox/..%2f..%2fetc%2fpasswd</d:href>
<d:propstat><d:prop><d:resourcetype/><d:getetag>"x"</d:getetag></d:prop>
<d:status>HTTP/1.1 200 OK</d:status></d:propstat>
</d:response></d:multistatus>"#;
        let entries = parse_multistatus(xml, ROOT).expect("parse");
        let e = &entries[0];
        assert!(!e.path.starts_with('/'), "must stay relative: {}", e.path);
        assert_eq!(e.path, "../../etc/passwd");
    }
}
