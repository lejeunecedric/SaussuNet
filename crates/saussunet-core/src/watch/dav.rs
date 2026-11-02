//! WebDAV HTTP client (plan 04-04): the only place Phase 4 talks HTTP.
//!
//! Thin reqwest wrapper — the 04-03 `multistatus` parser does all XML work.
//! Credential hygiene is the plan's BLOCKING threat-model rule: basic auth
//! rides ONLY on request headers (`.basic_auth()` from `SecretString::reveal()`
//! at request-build time), never in a URL (04-01 already rejects userinfo
//! config), and error Display carries `scheme://host/path` + reason only —
//! never userinfo, never response bodies.

use std::time::Duration;

use futures_util::StreamExt;
use url::Url;

use crate::config::{ResolvedWatch, SecretString};
use crate::error::{CoreError, Result};

use super::multistatus::{parse_multistatus, DavEntry};

/// Hard cap on multistatus response bodies, enforced BEFORE parsing
/// (XML-bomb / memory DoS control — roxmltree is DOM-based).
pub const MAX_MULTISTATUS_BYTES: u64 = 8 * 1024 * 1024;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_REDIRECTS: usize = 5;

/// PROPFIND body verbatim from RESEARCH §protocol_notes.
const PROPFIND_BODY: &str = r#"<?xml version="1.0"?>
<d:propfind xmlns:d="DAV:">
  <d:prop>
    <d:getetag/><d:getcontentlength/><d:resourcetype/>
  </d:prop>
</d:propfind>"#;

/// One client per watch source. `base` is the watch root URL (trailing slash
/// normalised — PROPFIND targets a collection); `auth` is applied
/// preemptively to every request when configured.
pub struct DavClient {
    client: reqwest::Client,
    base: Url,
    auth: Option<(String, SecretString)>,
}

impl DavClient {
    pub fn new(src: &ResolvedWatch) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::limited(MAX_REDIRECTS))
            .build()
            .map_err(|e| CoreError::Dav(format!("client build failed: {e}")))?;
        // Credential wholeness was validated at resolve() (04-01): either
        // both parts exist or neither.
        let auth = match (&src.username, &src.password) {
            (Some(u), Some(p)) => Some((u.clone(), p.clone())),
            _ => None,
        };
        let mut base = src.url.clone();
        if !base.path().ends_with('/') {
            // PROPFIND targets a collection; normalise so `join` keeps the
            // root segment instead of replacing it.
            base.set_path(&format!("{}/", base.path()));
        }
        Ok(Self { client, base, auth })
    }

    /// Depth:1 PROPFIND of `<base><rel>`; parsed entries, collection-self
    /// already skipped by the parser. Non-207 is an error (D-05 fallback is
    /// the caller's decision, not ours).
    pub async fn propfind_children(&self, rel: &str) -> Result<Vec<DavEntry>> {
        let url = self.url_for(rel)?;
        let target = display_target(&url);
        let method = reqwest::Method::from_bytes(b"PROPFIND").expect("PROPFIND is a valid method");
        let mut req = self
            .client
            .request(method, url.clone())
            .header("Depth", "1")
            .header(reqwest::header::CONTENT_TYPE, "application/xml");
        if let Some((u, p)) = &self.auth {
            req = req.basic_auth(u, Some(p.reveal()));
        }
        let resp = req
            .body(PROPFIND_BODY.to_string())
            .send()
            .await
            .map_err(|e| CoreError::Dav(format!("{target}: PROPFIND failed: {}", err_kind(&e))))?;
        if resp.status() != reqwest::StatusCode::MULTI_STATUS {
            return Err(CoreError::Dav(format!(
                "{target}: expected 207 Multi-Status, got {}",
                resp.status()
            )));
        }
        // Size cap BEFORE parse (memory control): declared Content-Length is
        // refused up front, a lying one is caught chunk-by-chunk.
        let bytes = read_capped(resp, MAX_MULTISTATUS_BYTES, &target).await?;
        let xml = String::from_utf8(bytes).map_err(|_| {
            CoreError::Dav(format!("{target}: multistatus body is not valid UTF-8"))
        })?;
        // root_path is the percent-ENCODED request path (parser decodes once).
        parse_multistatus(&xml, url.path())
    }

    /// GET with basic auth; aborts if the body exceeds `max_bytes` (streaming
    /// cap, fetch.rs precedent). Returns raw bytes — routing is the poller's.
    pub async fn get_bytes(&self, rel: &str, max_bytes: u64) -> Result<Vec<u8>> {
        let url = self.url_for(rel)?;
        let target = display_target(&url);
        let mut req = self.client.get(url.clone());
        if let Some((u, p)) = &self.auth {
            req = req.basic_auth(u, Some(p.reveal()));
        }
        let resp = req.send().await.map_err(|e| CoreError::Fetch {
            url: target.clone(),
            reason: format!("request failed: {}", err_kind(&e)),
        })?;
        if !resp.status().is_success() {
            return Err(CoreError::Fetch {
                url: target,
                reason: format!("HTTP {}", resp.status()),
            });
        }
        read_capped(resp, max_bytes, &target).await
    }

    /// Join `rel` (DECODED, root-relative) onto the base, percent-encoding
    /// each segment first (manual encoder: keep `[A-Za-z0-9._~-]`).
    fn url_for(&self, rel: &str) -> Result<Url> {
        if rel.is_empty() {
            return Ok(self.base.clone());
        }
        let encoded = rel
            .split('/')
            .map(encode_segment)
            .collect::<Vec<_>>()
            .join("/");
        self.base
            .join(&encoded)
            .map_err(|e| CoreError::Dav(format!("cannot build URL for watch path {rel:?}: {e}")))
    }
}

/// Error Display surface: `scheme://host/path` only — never userinfo
/// (04-01 rejects it in config and we never add it), never query secrets.
fn display_target(url: &Url) -> String {
    format!(
        "{}://{}{}",
        url.scheme(),
        url.host_str().unwrap_or("?"),
        url.path()
    )
}

/// Classify a reqwest failure into a leak-free reason (the raw error Display
/// embeds the request URL; ours is userinfo-free, but keep it minimal).
fn err_kind(e: &reqwest::Error) -> &'static str {
    if e.is_connect() {
        "connection failed"
    } else if e.is_timeout() {
        "timed out"
    } else {
        "request failed"
    }
}

/// Percent-encode one path segment: keep `[A-Za-z0-9._~-]`, encode every
/// other byte as `%XX` per UTF-8 byte.
fn encode_segment(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"._~-".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Read the body with a running size cap (mirrors fetch.rs `read_capped_body`
/// for bytes): declared over-cap is refused before reading; a lying or absent
/// Content-Length is caught chunk-by-chunk.
async fn read_capped(resp: reqwest::Response, max_bytes: u64, target: &str) -> Result<Vec<u8>> {
    if let Some(len) = resp.content_length() {
        if len > max_bytes {
            return Err(CoreError::Fetch {
                url: target.to_string(),
                reason: format!(
                    "declared body size {len} bytes exceeds the {max_bytes}-byte fetch cap"
                ),
            });
        }
    }
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| CoreError::Fetch {
            url: target.to_string(),
            reason: format!("body read failed: {}", err_kind(&e)),
        })?;
        if buf.len() as u64 + chunk.len() as u64 > max_bytes {
            return Err(CoreError::Fetch {
                url: target.to_string(),
                reason: format!("body exceeds the {max_bytes}-byte fetch cap"),
            });
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}
