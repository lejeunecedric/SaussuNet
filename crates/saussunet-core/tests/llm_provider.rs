//! Provider + mock test suite (LLMP-02, ROADMAP SC2). D-14 structural proof:
//! every test constructs `MockProvider` directly — zero config reads, zero
//! env vars, zero API keys — so hitting a real LLM from this suite is
//! structurally impossible. All typed calls go through the free
//! `generate_json_typed` helper against `&dyn LlmProvider`, proving the
//! trait is object-safe end-to-end (D-07).

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::json;

use saussunet_core::error::CoreError;
use saussunet_core::llm::{generate_json_typed, LlmProvider, MockProvider, Scripted};

/// Typed target proving the `generate_json_typed<T: DeserializeOwned>`
/// free-helper surface (D-07) without a generic trait method.
#[derive(Debug, Deserialize)]
struct Answer {
    a: u64,
}

#[derive(Debug, Deserialize)]
struct Fixed {
    fixed: bool,
}

fn schema() -> serde_json::Value {
    json!({"type": "object"})
}

#[tokio::test]
async fn script_consumed_in_order() {
    let provider = MockProvider::new(vec![
        Scripted::Ok(json!({"a": 1})),
        Scripted::Ok(json!({"a": 2})),
    ]);
    let first: Answer = generate_json_typed(&provider, "prompt one", &schema())
        .await
        .expect("first scripted value");
    let second: Answer = generate_json_typed(&provider, "prompt two", &schema())
        .await
        .expect("second scripted value");
    assert_eq!(first.a, 1);
    assert_eq!(second.a, 2);
    assert_eq!(provider.call_count(), 2);
    assert_eq!(provider.remaining().await, 0);
}

#[tokio::test]
async fn tolerant_extraction_from_fenced_prose() {
    // D-05: noisy prose + ```json fence parses to the typed value with NO
    // repair round (call_count stays 1).
    let provider = MockProvider::new(vec![Scripted::RawText(
        "Sure! Here you go:\n```json\n{\"a\":1}\n```\nHope that helps!".to_string(),
    )]);
    let answer: Answer = generate_json_typed(&provider, "give me an answer", &schema())
        .await
        .expect("fenced JSON extracts");
    assert_eq!(answer.a, 1);
    assert_eq!(provider.call_count(), 1);
}

#[tokio::test]
async fn repair_prompt_fixes_invalid_json_in_job() {
    // D-06: invalid output -> exactly one repair prompt -> typed success,
    // zero queue retries consumed.
    let provider = MockProvider::new(vec![
        Scripted::InvalidJson("not json {{{".to_string()),
        Scripted::Ok(json!({"fixed": true})),
    ]);
    let fixed: Fixed = generate_json_typed(&provider, "original prompt", &schema())
        .await
        .expect("repair round yields typed success");
    assert!(fixed.fixed);
    assert_eq!(provider.call_count(), 2);
    let prompts = provider.prompts().await;
    assert_eq!(prompts.len(), 2);
    assert!(
        prompts[1].contains("not valid JSON"),
        "second prompt must be the repair instruction, got: {:?}",
        prompts[1]
    );
}

#[tokio::test]
async fn repair_exhausted_fails_after_exactly_two_calls() {
    // D-06 bound: ONE repair, then LlmParse — never a third call.
    let provider = MockProvider::new(vec![
        Scripted::InvalidJson("nope".to_string()),
        Scripted::InvalidJson("still nope".to_string()),
    ]);
    let err = generate_json_typed::<Answer>(&provider, "prompt", &schema())
        .await
        .expect_err("both outputs unparseable");
    assert!(
        matches!(err, CoreError::LlmParse(_)),
        "expected LlmParse, got {err:?}"
    );
    assert_eq!(provider.call_count(), 2);
}

#[tokio::test]
async fn fail_500_surfaces_llm_error() {
    // D-15 retry-path seam: transport-style failure propagates untouched
    // (no repair round — repair is for unparseable output only).
    let provider = MockProvider::new(vec![Scripted::Fail { status: 500 }]);
    let err = provider
        .generate_json("prompt", &schema())
        .await
        .expect_err("scripted 500");
    match err {
        CoreError::Llm { status, url } => {
            assert_eq!(status, 500);
            assert_eq!(url, "mock://provider");
        }
        other => panic!("expected CoreError::Llm, got {other:?}"),
    }
    assert_eq!(provider.call_count(), 1);
}

#[tokio::test]
async fn hang_delays_then_fails() {
    // D-15 hang primitive for plan 02-04's heartbeat/reclaim tests.
    let provider = MockProvider::new(vec![Scripted::Hang(Duration::from_millis(50))]);
    let start = Instant::now();
    let err = provider
        .generate_json("prompt", &schema())
        .await
        .expect_err("hang must fail after sleeping");
    assert!(start.elapsed() >= Duration::from_millis(40));
    assert!(
        matches!(err, CoreError::Llm { status: 500, .. }),
        "expected Llm 500, got {err:?}"
    );
}

#[tokio::test]
async fn slow_ok_delays_then_succeeds() {
    // Bounded-pool gauge primitive for plan 02-04: delayed SUCCESS.
    let provider = MockProvider::new(vec![Scripted::SlowOk {
        delay: Duration::from_millis(50),
        value: json!({"a": 7}),
    }]);
    let start = Instant::now();
    let answer: Answer = generate_json_typed(&provider, "prompt", &schema())
        .await
        .expect("slow success");
    assert_eq!(answer.a, 7);
    assert!(start.elapsed() >= Duration::from_millis(40));
}

#[tokio::test]
async fn script_exhaustion_errors_instead_of_hanging() {
    // Threat model: finite script, hard error on exhaustion.
    let provider = MockProvider::new(vec![]);
    let err = provider
        .generate_json("prompt", &schema())
        .await
        .expect_err("empty script must error");
    assert!(
        matches!(err, CoreError::Llm { status: 0, .. }),
        "expected Llm status 0, got {err:?}"
    );
}

#[tokio::test]
async fn in_flight_gauge_observes_concurrent_calls() {
    // The max-concurrency gauge plan 02-04 asserts against: four delayed
    // successes driven through `Arc<dyn LlmProvider>` overlap in-flight.
    let provider = Arc::new(MockProvider::new(vec![
        Scripted::SlowOk {
            delay: Duration::from_millis(50),
            value: json!({"a": 1}),
        };
        4
    ]));
    let shared: Arc<dyn LlmProvider> = provider.clone();
    let s = schema();
    let f1 = generate_json_typed::<Answer>(&*shared, "prompt", &s);
    let f2 = generate_json_typed::<Answer>(&*shared, "prompt", &s);
    let f3 = generate_json_typed::<Answer>(&*shared, "prompt", &s);
    let f4 = generate_json_typed::<Answer>(&*shared, "prompt", &s);
    let (r1, r2, r3, r4) = tokio::join!(f1, f2, f3, f4);
    for outcome in [r1, r2, r3, r4] {
        assert_eq!(outcome.expect("slow success").a, 1);
    }
    assert_eq!(provider.call_count(), 4);
    assert_eq!(provider.max_concurrent_calls(), 4);
}
