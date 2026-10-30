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
pub async fn fetch_url(client: &reqwest::Client, url: &Url) -> Result<FetchResult> {
    let mut current = assert_public_http_url(url.as_str()).await?;
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
        let html = resp.text().await.map_err(|e| CoreError::Fetch {
            url: current.to_string(),
            reason: format!("body read failed: {e}"),
        })?;
        let (title, markdown) = extract::html_to_markdown(&html)?;
        return Ok(FetchResult {
            final_url: current.to_string(),
            sha256: sha256_hex(markdown.as_bytes()),
            title,
            markdown,
        });
    }
    Err(CoreError::Fetch {
        url: sanitize_for_error(url.as_str()),
        reason: format!("more than {MAX_REDIRECTS} redirects"),
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
}
