//! OpenAI-compatible HTTP provider (D-01, D-05 request side, D-08, PATTERNS §A2).
//!
//! `POST {base_url}/chat/completions` with bearer auth, non-streaming, and a
//! reqwest client-level timeout so a hung endpoint can never pin a worker
//! forever (D-04 resource bound). `response_format: {"type":"json_object"}`
//! is requested best-effort: on HTTP 400 the SAME request is retried once
//! without it — local llama.cpp/Ollama reject the field, and the trait-level
//! tolerant parse (D-05) makes their output work identically to cloud.
//!
//! D-03 secrecy: the api_key is revealed only into the bearer header at send
//! time. It appears in NO error (non-2xx errors carry status + url only —
//! response bodies can echo request headers, so bodies are never interpolated),
//! NO log, and NO Debug output (manual impl prints `api_key: "***"`).
//!
//! SSRF bound: the request URL is built solely from operator-configured
//! `base_url` at construction time — untrusted job payload content can only
//! ever be message body text, never influence the URL.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;

use crate::config::SecretString;
use crate::error::{CoreError, Result};

use super::LlmProvider;

/// Real OpenAI-compatible provider (cloud or local llama.cpp/Ollama — same
/// code path per D-01). Shares the trait default `generate_json` contract
/// (tolerant extract + one repair, D-05/D-06) by overriding only `complete`.
pub struct OpenAiCompat {
    base_url: String,
    model: String,
    /// Private: revealed only into the bearer header (D-03). Never printed.
    api_key: String,
    client: reqwest::Client,
}

impl std::fmt::Debug for OpenAiCompat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiCompat")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("api_key", &"***")
            .finish()
    }
}

impl OpenAiCompat {
    /// Build a provider for `base_url` (trailing slash trimmed) with a
    /// client-wide request `timeout` (default 300 s comes from config).
    pub fn new(
        base_url: &str,
        model: &str,
        api_key: &SecretString,
        timeout: Duration,
    ) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| CoreError::Config(format!("cannot build HTTP client: {e}")))?;
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            model: model.to_string(),
            api_key: api_key.reveal().to_string(),
            client,
        })
    }

    /// One POST attempt. `with_response_format` toggles the D-05 best-effort
    /// `response_format` flag (400 → caller retries once without it).
    async fn post_chat(
        &self,
        url: &str,
        prompt: &str,
        with_response_format: bool,
    ) -> std::result::Result<reqwest::Response, reqwest::Error> {
        let mut body = json!({
            "model": self.model,
            "messages": [{ "role": "user", "content": prompt }],
            "stream": false,
        });
        if with_response_format {
            body["response_format"] = json!({ "type": "json_object" });
        }
        self.client
            .post(url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
    }
}

#[async_trait]
impl LlmProvider for OpenAiCompat {
    async fn complete(&self, prompt: &str) -> Result<String> {
        let url = format!("{}/chat/completions", self.base_url);

        let mut resp = self
            .post_chat(&url, prompt, true)
            .await
            .map_err(|e| CoreError::Llm {
                status: 0,
                url: format!("{url}: {e}"),
            })?;

        // D-05 best-effort flag: endpoints that reject response_format with 400
        // (llama.cpp, some Ollama builds) get the SAME request once without it.
        if resp.status() == reqwest::StatusCode::BAD_REQUEST {
            resp = self
                .post_chat(&url, prompt, false)
                .await
                .map_err(|e| CoreError::Llm {
                    status: 0,
                    url: format!("{url}: {e}"),
                })?;
        }

        let status = resp.status();
        if !status.is_success() {
            // Deliberately NO body interpolation: response bodies can echo
            // request headers (incl. the bearer token) — D-03.
            return Err(CoreError::Llm {
                status: status.as_u16(),
                url,
            });
        }

        let body: serde_json::Value = resp.json().await.map_err(|e| CoreError::Llm {
            status: 0,
            url: format!("{url}: malformed success body: {e}"),
        })?;
        let content = body["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| CoreError::Llm {
                status: 0,
                url: format!("{url}: no message content in response"),
            })?;
        // The trait default extracts the `{...}` block tolerantly from here.
        Ok(content.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// D-03 acceptance: an LLM error string carries status + url only — never
    /// the api_key, never the Authorization scheme.
    #[test]
    fn llm_error_display_and_debug_never_leak_api_key() {
        let fake_key = "test-pass-9f3a";
        let err = CoreError::Llm {
            status: 401,
            url: "https://x/v1/chat/completions".into(),
        };
        let display = format!("{err}");
        let debug = format!("{err:?}");
        assert!(display.contains("401"));
        assert!(display.contains("https://x/v1/chat/completions"));
        for rendered in [display, debug] {
            assert!(!rendered.contains(fake_key));
            assert!(!rendered.contains("Bearer"));
        }
    }

    /// The provider's own Debug redacts the key (manual impl).
    #[test]
    fn provider_debug_redacts_api_key() {
        let secret = SecretString::new("test-pass-9f3a".to_string());
        let p = OpenAiCompat::new(
            "https://example.test/v1/",
            "test-model",
            &secret,
            Duration::from_secs(30),
        )
        .expect("client builds");
        let rendered = format!("{p:?}");
        assert!(rendered.contains("***"));
        assert!(rendered.contains("test-model"));
        assert!(!rendered.contains("test-pass-9f3a"));
        // trailing slash trimmed at construction (URL built once, SSRF bound)
        assert_eq!(p.base_url, "https://example.test/v1");
    }
}
