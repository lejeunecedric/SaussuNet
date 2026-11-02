//! RFC 4918 multistatus / RFC 6578 sync-report parser (pure, no HTTP).
//!
//! Namespace-aware DOM via roxmltree: no DTD fetching, undefined entities
//! error out — billion-laughs expansion is inert (threat model, 04-03).
//! Hrefs are percent-decoded ONCE here; everything downstream (state keys,
//! diffs) lives in decoded space; request URLs are rebuilt with `Url::join`
//! at the edge (04-04).

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
        let xml = format!(
            r#"<d:multistatus xmlns:d="DAV:"><d:response>
<d:href>/dav/vault-inbox/w.md</d:href>
<d:propstat><d:prop><d:resourcetype/><d:getetag>W/"abc"</d:getetag></d:prop>
<d:status>HTTP/1.1 200 OK</d:status></d:propstat>
</d:response></d:multistatus>"#
        );
        let entries = parse_multistatus(&xml, ROOT).expect("parse");
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
