//! UAT gap-closure (03-UAT test 9): the provider must send
//! `chat_template_kwargs: {"enable_thinking": false}` so thinking-heavy local
//! servers (Halogen xhigh default: ~410 s of reasoning before the answer)
//! answer inside the client timeout. Best-effort like `response_format`:
//! a 400 retries the SAME request with the field dropped.
//!
//! D-14 structural proof: a loopback stub server stands in for the endpoint —
//! zero config reads, zero env vars, zero API keys.

use std::io::{Read, Write};
use std::net::TcpListener;

use serde_json::{json, Value};

use saussunet_core::config::SecretString;
use saussunet_core::llm::{LlmProvider, OpenAiCompat};

/// One-shot stub: accepts `replies.len()` sequential HTTP requests, answering
/// each with the corresponding reply after recording its parsed JSON body.
/// Replies are consumed in order; a `400` reply simulates "server rejects the
/// fields present in that request".
fn stub_server(replies: Vec<u16>) -> (String, std::sync::mpsc::Receiver<Value>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("stub binds");
    let addr = listener.local_addr().expect("stub addr");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for status in replies {
            let Ok((mut stream, _)) = listener.accept() else {
                break;
            };
            // Read headers until \r\n\r\n, then exactly Content-Length bytes.
            let mut buf = Vec::new();
            let mut byte = [0u8; 1];
            while !buf.ends_with(b"\r\n\r\n") {
                if stream.read(&mut byte).unwrap_or(0) == 0 {
                    break;
                }
                buf.push(byte[0]);
            }
            let headers = String::from_utf8_lossy(&buf).to_lowercase();
            let len: usize = headers
                .lines()
                .find_map(|l| l.strip_prefix("content-length:").map(|v| v.trim()))
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            let mut body = vec![0u8; len];
            let mut read = 0;
            while read < len {
                read += stream.read(&mut body[read..]).unwrap_or(0);
            }
            let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let _ = tx.send(parsed);
            let payload = serde_json::to_vec(&json!({
                "choices": [{"message": {"content": "{\"ok\": true}"}}]
            }))
            .expect("stub body serializes");
            let head = format!(
                "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                if status == 200 { "OK" } else { "Bad Request" },
                payload.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&payload);
        }
    });
    (format!("http://{addr}/v1"), rx)
}

fn provider(base_url: &str) -> OpenAiCompat {
    OpenAiCompat::new(
        base_url,
        "stub-model",
        &SecretString::new("stub-key".to_string()),
        std::time::Duration::from_secs(10),
    )
    .expect("provider builds")
}

/// Happy path: the outbound body carries BOTH best-effort fields.
#[tokio::test]
async fn request_body_disables_thinking_best_effort() {
    let (base, rx) = stub_server(vec![200]);
    let out = provider(&base)
        .generate_json("prompt", &json!({}))
        .await
        .expect("stub 200");
    assert_eq!(out.get("ok"), Some(&json!(true)));
    let body = rx.recv().expect("one request recorded");
    assert_eq!(
        body["chat_template_kwargs"]["enable_thinking"],
        json!(false),
        "request must disable thinking, got: {body}"
    );
    assert_eq!(body["response_format"]["type"], json!("json_object"));
}

/// 400 fallback: servers that reject `chat_template_kwargs` get the SAME
/// request once without it (mirrors the response_format 400 dance).
#[tokio::test]
async fn thinking_field_dropped_on_400() {
    let (base, rx) = stub_server(vec![400, 200]);
    let out = provider(&base)
        .generate_json("prompt", &json!({}))
        .await
        .expect("retry without chat_template_kwargs succeeds");
    assert_eq!(out.get("ok"), Some(&json!(true)));
    let first = rx.recv().expect("first request");
    let second = rx.recv().expect("retry request");
    assert!(
        first.get("chat_template_kwargs").is_some(),
        "first attempt carries the field: {first}"
    );
    assert!(
        second.get("chat_template_kwargs").is_none(),
        "retry must drop the field: {second}"
    );
    assert_eq!(
        second["response_format"]["type"],
        json!("json_object"),
        "retry keeps response_format"
    );
    assert_eq!(first["messages"], second["messages"], "same prompt");
}

/// Both fields rejected: the ladder ends at the bare request (legacy
/// llama.cpp/Ollama behaviour preserved).
#[tokio::test]
async fn both_fields_dropped_when_server_rejects_everything() {
    let (base, rx) = stub_server(vec![400, 400, 200]);
    let out = provider(&base)
        .generate_json("prompt", &json!({}))
        .await
        .expect("bare retry succeeds");
    assert_eq!(out.get("ok"), Some(&json!(true)));
    let third = rx.recv().expect("req 1");
    let _ = rx.recv().expect("req 2");
    let bare = rx.recv().expect("req 3");
    assert!(third.get("chat_template_kwargs").is_some());
    assert!(bare.get("chat_template_kwargs").is_none());
    assert!(bare.get("response_format").is_none());
}
