//! SSRF-guarded URL fetch (PIPE-01, threat model 03-01).
//!
//! `assert_public_http_url` is the gate: scheme allowlist (http/https only),
//! rejection of loopback / RFC1918 / link-local / CGNAT / unspecified
//! addresses — both IP literals and every address a hostname resolves to —
//! enforced BEFORE any request leaves the process. `fetch_url` follows
//! redirects MANUALLY (reqwest `Policy::none()`) and re-runs the guard on
//! every hop, so a public host cannot bounce us into `169.254.169.254`.
//!
//! Accepted residual risk (v1, documented per plan): DNS-rebinding TOCTOU —
//! the guard resolves DNS, then reqwest resolves again at connect time and
//! could observe a different (private) record. Single-operator,
//! localhost-bound daemon; pinning the resolved IP at connect time is the
//! known hardening path if that threat class ever matters.

use std::net::{IpAddr, SocketAddr};

use url::Url;

use crate::error::{CoreError, Result};

use super::extract;

/// Maximum redirects followed manually (threat model: 5 hops).
const MAX_REDIRECTS: u8 = 5;
/// Request timeout for every hop (plan: 30 s).
const FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// Clean, identifiable User-Agent (no tool fingerprint soup).
const USER_AGENT: &str = "SaussuNet/0.1 (+local ingestion daemon)";
/// Default body size cap for guarded fetches. Mirrors
/// `config::default_max_fetch_bytes` (5 MiB); the queue handler swaps in
/// `RuntimeConfig.pipeline_max_fetch_bytes` once its signature carries config
/// (same documented seam as DEFAULT_MIN_SOURCES).
pub const DEFAULT_MAX_FETCH_BYTES: u64 = 5_242_880;

/// Result of a successful guarded fetch: article Markdown + provenance.
#[derive(Debug, Clone)]
pub struct FetchResult {
    /// Final URL after redirects, userinfo already stripped.
    pub final_url: String,
    /// Extracted article Markdown.
    pub markdown: String,
    /// sha256 hex of `markdown` (D-08 groundwork).
    pub sha256: String,
    /// Article title from the reader (may be empty for fallback pages).
    pub title: String,
}

/// True for every address the SSRF guard must refuse: loopback (127/8, ::1),
/// RFC1918 (10/8, 172.16/12, 192.168/16), link-local/metadata (169.254/16,
/// fe80::/10), CGNAT (100.64.0.0/10), unspecified (0.0.0.0, ::), and
/// IPv4-mapped IPv6 forms of the above.
pub(crate) fn ip_is_private(ip: IpAddr) -> bool {
    let v4_private = |o: [u8; 4]| {
        o[0] == 127 // loopback
            || o[0] == 10 // RFC1918
            || (o[0] == 172 && (16..=31).contains(&o[1])) // RFC1918
            || (o[0] == 192 && o[1] == 168) // RFC1918
            || (o[0] == 169 && o[1] == 254) // link-local / cloud metadata
            || (o[0] == 100 && (64..=127).contains(&o[1])) // CGNAT 100.64/10
            || o == [0, 0, 0, 0] // unspecified
    };
    match ip {
        IpAddr::V4(v4) => v4_private(v4.octets()),
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return v4_private(mapped.octets());
            }
            let o = v6.octets();
            // ::1 loopback, :: unspecified, fe80::/10 link-local
            o == [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]
                || o == [0u8; 16]
                || (o[0] == 0xfe && (o[1] & 0xc0) == 0x80)
        }
    }
}

/// Remove userinfo (`user:pass@`) from a URL in place — nothing secret may
/// ride into `FetchResult.final_url`, error Display, or logs (D-03 pattern).
fn strip_userinfo(url: &mut Url) {
    if !url.username().is_empty() {
        let _ = url.set_username("");
    }
    if url.password().is_some() {
        let _ = url.set_password(None);
    }
}

/// Best-effort sanitized URL string for error Display (never panics, never
/// prints userinfo).
fn sanitize_for_error(raw: &str) -> String {
    match Url::parse(raw) {
        Ok(mut u) => {
            strip_userinfo(&mut u);
            u.to_string()
        }
        Err(_) => "[unparseable url]".to_string(),
    }
}

/// The SSRF gate. Accepts only http(s) URLs whose host — literal IP or every
/// DNS answer — is a public address. Returns the sanitized URL (userinfo
/// stripped) for the caller to fetch.
///
/// Async because hostname allowlisting requires `tokio::net::lookup_host`.
pub async fn assert_public_http_url(raw: &str) -> Result<Url> {
    let mut url = Url::parse(raw).map_err(|e| CoreError::BlockedUrl {
        url: sanitize_for_error(raw),
        reason: format!("unparseable URL: {e}"),
    })?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(CoreError::BlockedUrl {
            url: sanitize_for_error(raw),
            reason: format!("scheme '{}' not allowed (http/https only)", url.scheme()),
        });
    }
    let host = url
        .host_str()
        .ok_or_else(|| CoreError::BlockedUrl {
            url: sanitize_for_error(raw),
            reason: "no host".into(),
        })?
        .to_string();
    strip_userinfo(&mut url);
    let blocked = |reason: String| CoreError::BlockedUrl {
        url: url.to_string(),
        reason,
    };

    // IPv6 hosts come back from url crate wrapped in [] — IpAddr::parse handles both.
    if let Ok(ip) = host.parse::<IpAddr>() {
        if ip_is_private(ip) {
            return Err(blocked(format!(
                "host {host} is a private/loopback address"
            )));
        }
        return Ok(url);
    }
    // Hostname: resolve and refuse if ANY answer is private (a mixed A/AAAA
    // record must not smuggle a loopback through happy-eyeballs).
    let port = url.port_or_known_default().unwrap_or(80);
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), port))
        .await
        .map_err(|e| blocked(format!("DNS resolution failed for {host}: {e}")))?
        .collect();
    if addrs.is_empty() {
        return Err(blocked(format!("DNS returned no addresses for {host}")));
    }
    if addrs.iter().any(|a| ip_is_private(a.ip())) {
        return Err(blocked(format!(
            "{host} resolves to a private/loopback address"
        )));
    }
    Ok(url)
}

/// Default reqwest client for guarded fetches: redirects OFF (the manual loop
/// in [`fetch_url`] re-guards every hop), 30 s timeout, clean UA.
pub fn default_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(FETCH_TIMEOUT)
        .user_agent(USER_AGENT)
        .build()
        .map_err(|e| CoreError::Fetch {
            url: String::new(),
            reason: format!("client build failed: {e}"),
        })
}

/// Fetch `url` (already guarded by the caller or re-guarded here), following
/// up to [`MAX_REDIRECTS`] redirects MANUALLY with the SSRF guard re-applied
/// to every hop. Body → article Markdown via [`extract::html_to_markdown`].
/// The body is read with a running cap of `max_bytes`: a declared
/// Content-Length over the cap is refused before reading, and a lying or
/// absent Content-Length is caught chunk-by-chunk (OOM guard, review 03 MEDIUM).
pub async fn fetch_url(client: &reqwest::Client, url: &Url, max_bytes: u64) -> Result<FetchResult> {
    let guarded = assert_public_http_url(url.as_str()).await?;
    fetch_url_unguarded(client, &guarded, max_bytes).await
}

/// Redirect loop + capped body read WITHOUT the entry guard (each redirect
/// hop is still re-guarded). `pub(crate)` so unit tests can exercise the size
/// cap against loopback fixture servers the public guard would refuse.
pub(crate) async fn fetch_url_unguarded(
    client: &reqwest::Client,
    current: &Url,
    max_bytes: u64,
) -> Result<FetchResult> {
    let mut current = current.clone();
    for _hop in 0..=MAX_REDIRECTS {
        let resp = client
            .get(current.clone())
            .send()
            .await
            .map_err(|e| CoreError::Fetch {
                url: current.to_string(),
                reason: format!("request failed: {e}"),
            })?;
        let status = resp.status();
        if status.is_redirection() {
            let location = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());
            let Some(location) = location else {
                return Err(CoreError::Fetch {
                    url: current.to_string(),
                    reason: format!("{status} redirect without Location header"),
                });
            };
            let next = current.join(&location).map_err(|e| CoreError::Fetch {
                url: current.to_string(),
                reason: format!("bad redirect target: {e}"),
            })?;
            // Re-guard EVERY hop (threat model: redirect into 169.254/16).
            current = assert_public_http_url(next.as_str()).await?;
            continue;
        }
        if !status.is_success() {
            return Err(CoreError::Fetch {
                url: current.to_string(),
                reason: format!("HTTP {status}"),
            });
        }
        let html = read_capped_body(resp, &current, max_bytes).await?;
        let (title, markdown) = extract::html_to_markdown(&html)?;
        return Ok(FetchResult {
            final_url: current.to_string(),
            sha256: sha256_hex(markdown.as_bytes()),
            title,
            markdown,
        });
    }
    Err(CoreError::Fetch {
        url: sanitize_for_error(current.as_str()),
        reason: format!("more than {MAX_REDIRECTS} redirects"),
    })
}

/// Read the response body with a running size cap. Refuses an over-cap
/// declared Content-Length up front, then accumulates chunks and aborts as
/// soon as the running total exceeds `max_bytes` — a lying or absent
/// Content-Length cannot stream an unbounded body into memory.
async fn read_capped_body(resp: reqwest::Response, url: &Url, max_bytes: u64) -> Result<String> {
    use futures_util::StreamExt;

    if let Some(len) = resp.content_length() {
        if len > max_bytes {
            return Err(CoreError::Fetch {
                url: sanitize_for_error(url.as_str()),
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
            url: sanitize_for_error(url.as_str()),
            reason: format!("body read failed: {e}"),
        })?;
        if buf.len() as u64 + chunk.len() as u64 > max_bytes {
            return Err(CoreError::Fetch {
                url: sanitize_for_error(url.as_str()),
                reason: format!("body exceeds the {max_bytes}-byte fetch cap"),
            });
        }
        buf.extend_from_slice(&chunk);
    }
    // Strict UTF-8 (matches the old reqwest text() behavior): a lossy decode
    // would silently corrupt the sha256 provenance of the extracted markdown.
    String::from_utf8(buf).map_err(|e| CoreError::Fetch {
        url: sanitize_for_error(url.as_str()),
        reason: format!("body is not valid UTF-8: {e}"),
    })
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
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
    fn ip_classification() {
        let priv4 = |s: &str| ip_is_private(s.parse().unwrap());
        assert!(priv4("127.0.0.1"));
        assert!(priv4("127.5.5.5")); // 127.0.0.0/8
        assert!(priv4("10.0.0.5"));
        assert!(priv4("172.16.0.1"));
        assert!(priv4("172.31.255.255"));
        assert!(!priv4("172.32.0.1")); // outside 172.16/12
        assert!(priv4("192.168.7.7"));
        assert!(priv4("169.254.169.254")); // cloud metadata
        assert!(priv4("100.64.0.1")); // CGNAT 100.64/10
        assert!(priv4("100.127.255.255"));
        assert!(!priv4("100.128.0.1")); // outside CGNAT range
        assert!(priv4("0.0.0.0"));
        assert!(!priv4("93.184.216.34"));
        assert!(ip_is_private("::1".parse().unwrap()));
        assert!(ip_is_private("fe80::1".parse().unwrap()));
        assert!(ip_is_private("::ffff:127.0.0.1".parse().unwrap())); // mapped loopback
        assert!(!ip_is_private("2606:4700::1".parse().unwrap()));
    }

    #[tokio::test]
    async fn ssrf_guard_rejects_private_targets() {
        for bad in [
            "file:///etc/passwd",
            "http://127.0.0.1/",
            "http://169.254.169.254/latest/meta-data/",
            "http://10.0.0.5/",
            "http://[::1]/",
            "gopher://example.com/",
            "http://user:pass@127.0.0.1/admin",
        ] {
            let err = assert_public_http_url(bad)
                .await
                .expect_err(&format!("{bad} must be refused before any request"));
            assert!(
                matches!(err, CoreError::BlockedUrl { .. }),
                "{bad}: expected BlockedUrl, got {err:?}"
            );
        }
    }

    #[tokio::test]
    async fn ssrf_guard_strips_userinfo_from_returned_url() {
        let ok = assert_public_http_url("https://user:secret@example.com/")
            .await
            .expect("public host must pass");
        assert!(ok.username().is_empty());
        assert!(ok.password().is_none());
        assert!(!ok.to_string().contains("secret"));
    }

    #[tokio::test]
    async fn ssrf_guard_accepts_public_https() {
        let ok = assert_public_http_url("https://example.com/")
            .await
            .expect("example.com must pass the guard");
        assert_eq!(ok.scheme(), "https");
    }

    // ---- max_fetch_bytes enforcement (03-08 review MEDIUM) ----------------
    //
    // fetch_url's SSRF guard blocks loopback, so these tests call the
    // internal unguarded core directly — the guard is covered above.

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Serve one canned HTTP response on an ephemeral loopback port, return it.
    async fn serve_once(response: &'static [u8]) -> Url {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = sock.read(&mut buf).await; // drain request line
            let _ = sock.write_all(response).await;
            let _ = sock.shutdown().await;
        });
        Url::parse(&format!("http://{addr}/page")).unwrap()
    }

    const SMALL_HTML: &str = "<html><body><p>tiny</p></body></html>";

    #[tokio::test]
    async fn fetch_rejects_declared_content_length_over_cap() {
        let body = "x".repeat(5_000);
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/html\r\n\r\n{body}",
            body.len()
        );
        let url = serve_once(resp.into_bytes().leak()).await;
        let err = fetch_url_unguarded(&default_client().unwrap(), &url, 1_000)
            .await
            .expect_err("declared 5000 bytes over a 1000 cap must fail");
        assert!(
            matches!(&err, CoreError::Fetch { reason, .. } if reason.contains("exceeds")),
            "expected size-cap Fetch error, got {err:?}"
        );
    }

    #[tokio::test]
    async fn fetch_rejects_body_over_cap_without_content_length() {
        // No Content-Length (connection-close framing): 5000 bytes stream in
        // and only the running cap can stop them.
        let body = "x".repeat(5_000);
        let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n{body}");
        let url = serve_once(resp.into_bytes().leak()).await;
        let err = fetch_url_unguarded(&default_client().unwrap(), &url, 1_000)
            .await
            .expect_err("lying Content-Length must not bypass the running cap");
        assert!(
            matches!(&err, CoreError::Fetch { reason, .. } if reason.contains("exceeds")),
            "expected size-cap Fetch error, got {err:?}"
        );
    }

    #[tokio::test]
    async fn fetch_accepts_body_under_cap() {
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/html\r\n\r\n{SMALL_HTML}",
            SMALL_HTML.len()
        );
        let url = serve_once(resp.into_bytes().leak()).await;
        let got = fetch_url_unguarded(&default_client().unwrap(), &url, 1_000)
            .await
            .expect("small body must pass");
        assert!(!got.markdown.is_empty());
        assert_eq!(got.sha256.len(), 64);
    }
}
