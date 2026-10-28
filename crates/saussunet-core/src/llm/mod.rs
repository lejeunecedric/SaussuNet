//! LLM provider abstraction (LLMP-01/LLMP-02, PATTERNS §A1).
//!
//! [`LlmProvider`] is the single interchangeable seam between the pipeline and
//! whatever answers completions (cloud OpenAI-compatible endpoint, local
//! llama.cpp/Ollama, or the deterministic [`mock`] used by every test — D-14).
//!
//! Design contracts locked by Phase 2 context:
//! - **D-07:** workers call [`LlmProvider::generate_json`] only — never raw
//!   strings. Typed deserialization for callers goes through the free helper
//!   [`generate_json_typed`], NOT a generic trait method: a generic `<T>` on a
//!   trait method is rustc E0038 and would break `Arc<dyn LlmProvider>`
//!   consumers (plans 02-04/02-05 inject the trait object).
//! - **D-05:** output is ALWAYS parsed tolerantly — the `{...}` block is
//!   extracted from noisy prose (code fences, chatter) before parsing, so
//!   local models that ignore `response_format` behave identically to cloud.
//! - **D-06:** on unparseable output exactly ONE repair prompt is sent in-job;
//!   a success there consumes ZERO queue retries.
//! - **D-08:** non-streaming one-shot completions.
//!
//! Threat model: implementations must treat `prompt` strictly as message
//! content (no shell/eval, no URL fetching from prompt text); the repair
//! prompt echoes the model's own prior output bounded to 4000 chars
//! (token-bomb guard); parsed output only ever lands in typed values.

use async_trait::async_trait;
use serde::de::DeserializeOwned;

use crate::error::{CoreError, Result};

pub mod mock;
pub mod openai_compat;
pub use mock::{MockProvider, Scripted};
pub use openai_compat::OpenAiCompat;

/// Single active provider selection from the resolved `[llm]` section (D-02):
/// exactly one provider is ever constructed. `"mock"` builds an empty-script
/// [`MockProvider`] — daemon mock mode only proves plumbing (every scripted
/// response lives in tests, never in config). `"openai-compatible"` builds
/// [`OpenAiCompat`] and requires an api_key (file or `SAUSSUNET_LLM__API_KEY`,
/// D-03). Anything else is a config error.
pub fn provider_from_config(
    rt: &crate::config::RuntimeConfig,
) -> Result<std::sync::Arc<dyn LlmProvider>> {
    use std::sync::Arc;
    match rt.llm_provider.as_str() {
        "mock" => Ok(Arc::new(MockProvider::new(vec![]))),
        "openai-compatible" => {
            let api_key = rt.llm_api_key.as_ref().ok_or_else(|| {
                CoreError::Config(
                    "llm api_key required: set [llm].api_key or SAUSSUNET_LLM__API_KEY".to_string(),
                )
            })?;
            Ok(Arc::new(OpenAiCompat::new(
                &rt.llm_base_url,
                &rt.llm_model,
                api_key,
                std::time::Duration::from_secs(rt.llm_timeout_secs),
            )?))
        }
        other => Err(CoreError::Config(format!("unknown llm provider: {other}"))),
    }
}

/// WR-03: should a daemon with this resolved config drain the job queue?
///
/// `provider = "mock"` (the shipped default) builds an EMPTY-script
/// [`MockProvider`], so every claimed job would fail "mock script exhausted"
/// and dead-letter after 4 invisible retries — indistinguishable from a real
/// outage in the dead-letter table. The honest behavior is to NOT spawn
/// workers in mock daemon mode: jobs stay `pending` until a real provider is
/// configured. Tests drive workers with scripted mocks directly via
/// `spawn_workers` — this gate lives at the daemon boot boundary only.
pub fn daemon_drains_queue(rt: &crate::config::RuntimeConfig) -> bool {
    rt.llm_provider != "mock"
}

/// Object-safe LLM seam (ARCHITECTURE.md Pattern 2 transposed). Implementors
/// override [`complete`](Self::complete); the default
/// [`generate_json`](Self::generate_json) implements the D-05/D-06 contract
/// for free. Share one instance via `Arc<dyn LlmProvider>` (vault/mod.rs
/// discipline — providers are NOT Clone on purpose).
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Raw one-shot, non-streaming completion (D-08). The low-level seam that
    /// provider implementations override; workers should never call this —
    /// they use [`generate_json`] / [`generate_json_typed`] (D-07).
    async fn complete(&self, prompt: &str) -> Result<String>;

    /// Prompt goes in, structured JSON comes out. Default body: `complete` →
    /// [`extract_json_block`] → parse; on failure exactly ONE repair prompt
    /// via [`repair_prompt`] is sent in-job (D-06 — no queue retry involved);
    /// still unparseable → [`CoreError::LlmParse`].
    async fn generate_json(
        &self,
        prompt: &str,
        schema: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        let raw = self.complete(prompt).await?;
        if let Some(block) = extract_json_block(&raw) {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(block) {
                return Ok(value);
            }
        }
        // D-06: single in-job repair round — exactly one second `complete`
        // call, no queue-retry involvement.
        let repair = repair_prompt(prompt, &raw, schema);
        let repaired = self.complete(&repair).await?;
        let block = extract_json_block(&repaired).ok_or_else(|| {
            CoreError::LlmParse("no JSON object found in repaired output".to_string())
        })?;
        serde_json::from_str(block)
            .map_err(|e| CoreError::LlmParse(format!("repaired output is not valid JSON: {e}")))
    }
}

/// Typed surface of D-07 without breaking dyn-compatibility: the generic
/// parameter lives on this FREE function, never on a trait method.
pub async fn generate_json_typed<T: DeserializeOwned>(
    provider: &dyn LlmProvider,
    prompt: &str,
    schema: &serde_json::Value,
) -> Result<T> {
    let value = provider.generate_json(prompt, schema).await?;
    serde_json::from_value(value)
        .map_err(|e| CoreError::LlmParse(format!("typed deserialization failed: {e}")))
}

/// D-05 tolerant extraction: pull the balanced `{...}` block out of noisy
/// model output (prose, ```json fences, trailing chatter).
///
/// Tries the whole (trimmed) string first; otherwise scans from the first
/// `{` with a brace-depth counter that is string-literal aware (quoted
/// ranges are skipped honoring backslash escapes) to its matching `}`.
/// Returns `None` when no balanced object exists.
pub fn extract_json_block(raw: &str) -> Option<&str> {
    let trimmed = raw.trim();
    if serde_json::from_str::<serde_json::Value>(trimmed).is_ok() {
        return Some(trimmed);
    }

    let bytes = raw.as_bytes();
    let start = raw.find('{')?;
    let mut depth: usize = 0;
    let mut in_string = false;
    let mut escaped = false;
    for i in start..bytes.len() {
        let c = bytes[i];
        if in_string {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_string = false;
            }
            continue;
        }
        match c {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    // `{` and `}` are ASCII, so these byte offsets are
                    // always char boundaries.
                    return Some(&raw[start..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Upper bound on the model's echoed prior output inside a repair prompt
/// (threat model: bounded echo prevents token-bomb amplification).
const REPAIR_ECHO_MAX_CHARS: usize = 4000;

/// D-06 repair prompt: original prompt + instruction + the model's own bad
/// output truncated to [`REPAIR_ECHO_MAX_CHARS`] chars.
fn repair_prompt(prompt: &str, bad_output: &str, schema: &serde_json::Value) -> String {
    let truncated: String = bad_output.chars().take(REPAIR_ECHO_MAX_CHARS).collect();
    format!(
        "{prompt}\n\n---\nYour previous output was not valid JSON matching this schema: {schema}\n\
         Output ONLY the corrected JSON object.\nYour previous output was:\n{truncated}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Compile-time dyn-compatibility guard (D-07): if a generic trait method
    /// ever creeps back in, this line fails to build with rustc E0038.
    #[test]
    fn trait_is_dyn_compatible() {
        let _: Option<&dyn LlmProvider> = None;
    }

    #[test]
    fn mock_daemon_does_not_drain_queue() {
        // WR-03: the default mock provider must not drain the queue — an
        // empty-script mock would dead-letter every job it claims.
        let cfg: crate::config::FileConfig =
            toml::from_str("[llm]\nprovider = \"mock\"\n").expect("parses");
        let rt = cfg.resolve().expect("resolve");
        assert!(!daemon_drains_queue(&rt));
    }

    #[test]
    fn real_provider_daemon_drains_queue() {
        let cfg: crate::config::FileConfig = toml::from_str(
            "[llm]\nprovider = \"openai-compatible\"\napi_key = \"test-pass-9f3a\"\n",
        )
        .expect("parses");
        let rt = cfg.resolve().expect("resolve");
        assert!(daemon_drains_queue(&rt));
    }

    #[test]
    fn extract_pure_json() {
        let raw = r#"{"a": 1}"#;
        assert_eq!(extract_json_block(raw), Some(raw));
    }

    #[test]
    fn extract_prose_wrapped() {
        let raw = r#"Sure, here you go: {"a": 1} Hope that helps!"#;
        assert_eq!(extract_json_block(raw), Some(r#"{"a": 1}"#));
    }

    #[test]
    fn extract_fenced_block() {
        let raw = "```json\n{\"a\": 1}\n```";
        assert_eq!(extract_json_block(raw), Some(r#"{"a": 1}"#));
    }

    #[test]
    fn extract_nested_objects() {
        let raw = r#"answer: {"a": {"b": [1, {"c": 2}]}, "s": "}{"} done"#;
        assert_eq!(
            extract_json_block(raw),
            Some(r#"{"a": {"b": [1, {"c": 2}]}, "s": "}{"}"#)
        );
    }

    #[test]
    fn extract_trailing_chatter() {
        let raw = r#"{"a": 1} and then some more words"#;
        assert_eq!(extract_json_block(raw), Some(r#"{"a": 1}"#));
    }

    #[test]
    fn extract_unterminated_brace_is_none() {
        assert_eq!(extract_json_block("oops {{{ no close"), None);
        assert_eq!(extract_json_block("no braces at all"), None);
    }

    #[test]
    fn repair_prompt_truncates_bad_output_at_4000_chars() {
        let long = "x".repeat(5000);
        let prompt = repair_prompt("orig", &long, &json!({"type": "object"}));
        assert!(prompt.contains("not valid JSON"));
        assert!(prompt.contains("orig"));
        // The echoed output is bounded to exactly 4000 x's; the rest of the
        // prompt is fixed instruction text.
        assert_eq!(prompt.matches('x').count(), 4000);
        assert!(prompt.chars().count() < 4000 + 512);
    }
}
