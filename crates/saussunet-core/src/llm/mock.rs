//! Deterministic scripted provider (D-13/D-15, PATTERNS §A3) — the test
//! double that drives the entire pipeline with zero real LLM calls (D-14).
//!
//! `MockProvider` is fed a scripted [`Scripted`] queue consumed in order.
//! It overrides only [`LlmProvider::complete`], so the trait-level default
//! `generate_json` (tolerant parse + single repair round) is exercised
//! identically to a real provider. Interior mutability uses
//! `tokio::sync::Mutex` — the repo's precedent is `vault/mod.rs` `write_lock`.
//!
//! NOT `Clone` on purpose: share one instance as `Arc<dyn LlmProvider>`
//! (vault/mod.rs discipline). Script exhaustion is a hard error, never a
//! hang (threat model: bounded resources).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;

use crate::error::{CoreError, Result};

use super::LlmProvider;

/// One scripted provider behaviour (D-13 success paths, D-15 failure paths).
#[derive(Clone, Debug)]
pub enum Scripted {
    /// Immediate success: the value serialized as JSON text.
    Ok(serde_json::Value),
    /// Success after `delay` — the bounded-pool gauge primitive (plan 02-04
    /// needs delayed *successes*, which `Hang` cannot provide).
    SlowOk {
        delay: Duration,
        value: serde_json::Value,
    },
    /// Arbitrary raw text (prose/fences around JSON) — exercises D-05
    /// tolerant extraction without a repair round.
    RawText(String),
    /// Unparseable garbage returned as a success — exercises the D-06
    /// trait-level repair path.
    InvalidJson(String),
    /// HTTP-5xx-equivalent provider error (retry-path seam for D-15).
    Fail { status: u16 },
    /// Sleeps `Duration` then fails 500 — the hang primitive for
    /// heartbeat/reclaim tests (plan 02-04).
    Hang(Duration),
}

/// Scripted mock provider (D-13). Construct directly in tests — never via
/// config (D-14: calling a real LLM from tests is structurally impossible).
#[derive(Debug)]
pub struct MockProvider {
    script: tokio::sync::Mutex<VecDeque<Scripted>>,
    /// Every prompt the provider received, in order — lets tests assert the
    /// repair prompt content without a bespoke Scripted variant.
    prompts: tokio::sync::Mutex<Vec<String>>,
    calls: AtomicUsize,
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
}

impl MockProvider {
    pub fn new(script: Vec<Scripted>) -> Self {
        Self {
            script: tokio::sync::Mutex::new(VecDeque::from(script)),
            prompts: tokio::sync::Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
            in_flight: AtomicUsize::new(0),
            max_in_flight: AtomicUsize::new(0),
        }
    }

    /// Total `complete` calls served so far.
    pub fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    /// Peak number of simultaneously in-flight calls (the gauge plan 02-04
    /// uses to prove the D-04 pool bound).
    pub fn max_concurrent_calls(&self) -> usize {
        self.max_in_flight.load(Ordering::SeqCst)
    }

    /// Script entries not yet consumed.
    pub async fn remaining(&self) -> usize {
        self.script.lock().await.len()
    }

    /// All prompts seen, in order (repair-prompt assertions).
    pub async fn prompts(&self) -> Vec<String> {
        self.prompts.lock().await.clone()
    }

    /// Pop the next script entry and realise its behaviour. The sleeps for
    /// `SlowOk`/`Hang` run inside the caller's in-flight guard so the
    /// concurrency gauge observes delayed responses.
    async fn step(&self) -> Result<String> {
        let next = self.script.lock().await.pop_front();
        match next {
            // CoreError::Llm carries no message field (D-03 minimalism), so
            // the exhaustion notice rides in `url`.
            None => Err(CoreError::Llm {
                status: 0,
                url: "mock script exhausted".to_string(),
            }),
            Some(Scripted::Ok(value)) => Ok(value.to_string()),
            Some(Scripted::SlowOk { delay, value }) => {
                tokio::time::sleep(delay).await;
                Ok(value.to_string())
            }
            Some(Scripted::RawText(text)) => Ok(text),
            Some(Scripted::InvalidJson(garbage)) => Ok(garbage),
            Some(Scripted::Fail { status }) => Err(CoreError::Llm {
                status,
                url: "mock://provider".to_string(),
            }),
            Some(Scripted::Hang(duration)) => {
                tokio::time::sleep(duration).await;
                Err(CoreError::Llm {
                    status: 500,
                    url: "mock://provider".to_string(),
                })
            }
        }
    }
}

#[async_trait]
impl LlmProvider for MockProvider {
    /// Overrides only the raw seam (D-08 one-shot): the trait default
    /// `generate_json` adds extraction + repair on top, exactly as it will
    /// for the HTTP provider.
    async fn complete(&self, prompt: &str) -> Result<String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.prompts.lock().await.push(prompt.to_string());
        let now_in_flight = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_in_flight
            .fetch_max(now_in_flight, Ordering::SeqCst);
        let outcome = self.step().await;
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        outcome
    }
}
