//! D-01 live-endpoint validation (cloud OpenAI-compatible first, then the
//! SAME path against local Ollama — no local-specific logic).
//!
//! This test is `#[ignore]`d and reads credentials from the environment only
//! inside the test body, so CI (which has no keys and never passes `--ignored`)
//! structurally cannot call a real LLM (D-14). Manual run:
//!
//! ```text
//! SAUSSUNET_LLM__BASE_URL=https://api.openai.com/v1 \
//! SAUSSUNET_LLM__MODEL=<model> \
//! SAUSSUNET_LLM__API_KEY=*** \
//!   cargo test -p saussunet-core --test llm_live -- --ignored
//! ```

use std::time::Duration;

use serde_json::json;

use saussunet_core::config::SecretString;
use saussunet_core::llm::{LlmProvider, OpenAiCompat};

/// One round-trip through the real provider: the non-generic
/// `generate_json(&prompt, &schema)` (dyn-compatible, plan 02-03) must come
/// back with `{"ok": true}` — proving bearer auth, chat/completions wiring,
/// and the tolerant parse against a live endpoint.
#[tokio::test]
#[ignore = "requires SAUSSUNET_LLM__API_KEY + SAUSSUNET_LLM__BASE_URL + SAUSSUNET_LLM__MODEL"]
async fn live_endpoint_returns_valid_json() {
    let Ok(key) = std::env::var("SAUSSUNET_LLM__API_KEY") else {
        panic!("skipped: SAUSSUNET_LLM__API_KEY not set — export it (plus BASE_URL + MODEL) to run this live test");
    };
    let base_url =
        std::env::var("SAUSSUNET_LLM__BASE_URL").expect("SAUSSUNET_LLM__BASE_URL must be set");
    let model = std::env::var("SAUSSUNET_LLM__MODEL").expect("SAUSSUNET_LLM__MODEL must be set");

    let provider = OpenAiCompat::new(
        &base_url,
        &model,
        &SecretString::new(key),
        Duration::from_secs(120),
    )
    .expect("provider builds");

    let schema = json!({});
    let out = provider
        .generate_json("Return exactly this JSON: {\"ok\": true}", &schema)
        .await
        .expect("live generate_json call failed");
    assert_eq!(out.get("ok"), Some(&json!(true)));
}
