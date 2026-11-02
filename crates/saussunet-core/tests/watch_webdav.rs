//! Phase 4 walking skeleton (plan 04-04): DavClient against a STATEFUL raw
//! TcpListener WebDAV stub (pattern: tests/llm_provider_thinking.rs — std::io
//! only, zero web frameworks, zero real network beyond loopback).
//!
//! Task 1 covers the client surface (propfind_children / get_bytes, size cap,
//! credential hygiene, percent-encoding roundtrip). Task 2 adds poll_once
//! integration tests at the bottom of this file.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use saussunet_core::config::{FileConfig, ResolvedWatch};
use saussunet_core::watch::dav::DavClient;

// ---------------------------------------------------------------------------
// Stateful stub WebDAV server (raw std::net + threads — no web framework)
// ---------------------------------------------------------------------------

struct StubFile {
    bytes: Vec<u8>,
    etag: String,
}

/// Handle into the running stub. Tree keys are DECODED vault-relative paths;
/// directory keys end with '/'.
#[derive(Clone)]
pub struct StubHandle {
    tree: Arc<Mutex<HashMap<String, StubFile>>>,
    log: Arc<Mutex<Vec<(String, String, Option<String>, String)>>>,
    /// 04-07 knob (unused in 04-04): flip to make POST answer 207 instead of
    /// the D-05 fallback 400.
    sync_mode: Arc<AtomicBool>,
}

impl StubHandle {
    pub fn insert(&self, path: &str, bytes: &[u8], etag: &str) {
        self.tree.lock().unwrap().insert(
            path.to_string(),
            StubFile {
                bytes: bytes.to_vec(),
                etag: etag.to_string(),
            },
        );
    }
    pub fn remove(&self, path: &str) {
        self.tree.lock().unwrap().remove(path);
    }
    pub fn set_etag(&self, path: &str, etag: &str) {
        if let Some(f) = self.tree.lock().unwrap().get_mut(path) {
            f.etag = etag.to_string();
        }
    }
    /// Recorded requests: (method, decoded path, Depth header, body).
    pub fn requests(&self) -> Vec<(String, String, Option<String>, String)> {
        self.log.lock().unwrap().clone()
    }
    #[allow(dead_code)]
    pub fn set_sync_mode(&self, on: bool) {
        self.sync_mode.store(on, Ordering::SeqCst);
    }
}

/// Start the stub; returns its base URL (`http://127.0.0.1:<port>/`) + handle.
pub fn stub_webdav() -> (String, StubHandle) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("stub binds");
    let addr = listener.local_addr().expect("stub addr");
    let tree: Arc<Mutex<HashMap<String, StubFile>>> = Arc::new(Mutex::new(HashMap::new()));
    let log: Arc<Mutex<Vec<(String, String, Option<String>, String)>>> =
        Arc::new(Mutex::new(Vec::new()));
    let sync_mode = Arc::new(AtomicBool::new(false));
    let (tree_t, log_t, sync_t) = (Arc::clone(&tree), Arc::clone(&log), Arc::clone(&sync_mode));
    std::thread::spawn(move || loop {
        let Ok((mut stream, _)) = listener.accept() else {
            break;
        };
        let tree = Arc::clone(&tree_t);
        let log = Arc::clone(&log_t);
        let sync_mode = Arc::clone(&sync_t);
        std::thread::spawn(move || {
            let _ = handle_conn(&mut stream, &tree, &log, &sync_mode);
        });
    });
    (
        format!("http://{addr}/"),
        StubHandle {
            tree,
            log,
            sync_mode,
        },
    )
}

fn handle_conn(
    stream: &mut std::net::TcpStream,
    tree: &Mutex<HashMap<String, StubFile>>,
    log: &Mutex<Vec<(String, String, Option<String>, String)>>,
    sync_mode: &AtomicBool,
) -> std::io::Result<()> {
    // Read headers until \r\n\rn, then exactly Content-Length bytes.
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte).unwrap_or(0) == 0 {
            return Ok(());
        }
        buf.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&buf).to_string();
    let mut lines = head.lines();
    let mut reqline = || String::from(lines.next().unwrap_or(""));
    let request_line = reqline();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let raw_path = parts.next().unwrap_or("/").to_string();
    let mut content_length = 0usize;
    let mut depth = None;
    for line in lines {
        let lower = line.to_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        } else if let Some(v) = lower.strip_prefix("depth:") {
            depth = Some(v.trim().to_string());
        }
    }
    let mut body = vec![0u8; content_length];
    let mut read = 0;
    while read < content_length {
        let n = stream.read(&mut body[read..])?;
        if n == 0 {
            break;
        }
        read += n;
    }
    let path = percent_decode(raw_path.as_bytes());
    log.lock().unwrap().push((
        method.clone(),
        path.clone(),
        depth,
        String::from_utf8_lossy(&body).to_string(),
    ));

    let (status, headers, resp_body): (u16, Vec<String>, Vec<u8>) = match method.as_str() {
        "PROPFIND" => propfind_response(tree, &raw_path, &path),
        "GET" => get_response(tree, &path),
        "POST" => {
            // D-05 fallback path: sync unsupported by default (04-07 flips
            // sync_mode to serve canned 207 sync-reports).
            if sync_mode.load(Ordering::SeqCst) {
                (
                    207,
                    vec!["Content-Type: application/xml".to_string()],
                    b"<d:multistatus xmlns:d=\"DAV:\"></d:multistatus>".to_vec(),
                )
            } else {
                (400, vec![], b"Not Implemented".to_vec())
            }
        }
        _ => (405, vec![], b"Method Not Allowed".to_vec()),
    };
    let reason = match status {
        200 => "OK",
        207 => "Multi-Status",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Method Not Allowed",
    };
    let mut head_out = format!("HTTP/1.1 {status} {reason}\r\n");
    for h in &headers {
        head_out.push_str(h);
        head_out.push_str("\r\n");
    }
    head_out.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n",
        resp_body.len()
    ));
    stream.write_all(head_out.as_bytes())?;
    stream.write_all(&resp_body)?;
    Ok(())
}

fn propfind_response(
    tree: &Mutex<HashMap<String, StubFile>>,
    raw_path: &str,
    decoded_path: &str,
) -> (u16, Vec<String>, Vec<u8>) {
    // Tree key for the walked collection: "" for root, "papers/" for a subdir.
    let key = decoded_path.trim_start_matches('/').to_string();
    let key = if key.is_empty() || key.ends_with('/') {
        key
    } else {
        format!("{key}/")
    };
    let guard = tree.lock().unwrap();
    if !key.is_empty() && !guard.contains_key(&key) {
        return (404, vec![], b"Not Found".to_vec());
    }
    // One-level children of `key`, sorted for determinism.
    let mut children: Vec<String> = guard
        .keys()
        .filter(|k| {
            if key.is_empty() {
                let rest = k.as_str();
                (!rest.ends_with('/') && !rest.contains('/'))
                    || (rest.ends_with('/') && !rest[..rest.len() - 1].contains('/'))
            } else if let Some(rest) = k.strip_prefix(&key) {
                !rest.is_empty()
                    && ((!rest.ends_with('/') && !rest.contains('/'))
                        || (rest.ends_with('/') && !rest[..rest.len() - 1].contains('/')))
            } else {
                false
            }
        })
        .cloned()
        .collect();
    children.sort();

    let self_href = if raw_path.is_empty() { "/" } else { raw_path };
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<d:multistatus xmlns:d=\"DAV:\">\n",
    );
    // Self first (Depth:1 includes self — the parser skips it).
    xml.push_str(&format!(
        "  <d:response><d:href>{self_href}</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>\n"
    ));
    for child in children {
        let name = child.strip_prefix(&key).unwrap_or(&child);
        let (name_body, dir_slash) = match name.strip_suffix('/') {
            Some(n) => (n, "/"),
            None => (name, ""),
        };
        let name_enc = encode_segment(name_body);
        let href = format!("{}/{name_enc}{dir_slash}", self_href.trim_end_matches('/'));
        let entry = guard.get(&child).expect("key just listed");
        if child.ends_with('/') {
            xml.push_str(&format!(
                "  <d:response><d:href>{href}</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>\n"
            ));
        } else {
            xml.push_str(&format!(
                "  <d:response><d:href>{href}</d:href><d:propstat><d:prop><d:resourcetype/><d:getetag>{}</d:getetag><d:getcontentlength>{}</d:getcontentlength></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>\n",
                entry.etag,
                entry.bytes.len()
            ));
        }
    }
    xml.push_str("</d:multistatus>");
    (
        207,
        vec!["Content-Type: application/xml".to_string()],
        xml.into_bytes(),
    )
}

fn get_response(
    tree: &Mutex<HashMap<String, StubFile>>,
    decoded_path: &str,
) -> (u16, Vec<String>, Vec<u8>) {
    let key = decoded_path.trim_start_matches('/');
    let guard = tree.lock().unwrap();
    match guard.get(key) {
        Some(f) if !key.is_empty() => (
            200,
            vec![
                "Content-Type: application/octet-stream".to_string(),
                format!("ETag: {}", f.etag),
            ],
            f.bytes.clone(),
        ),
        _ => (404, vec![], b"Not Found".to_vec()),
    }
}

/// Percent-encode one path segment: keep `[A-Za-z0-9._~-]`, encode per byte.
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

fn percent_decode(input: &[u8]) -> String {
    let mut out: Vec<u8> = Vec::new();
    let mut i = 0;
    while i < input.len() {
        if input[i] == b'%' && i + 2 < input.len() {
            let hi = (input[i + 1] as char).to_digit(16);
            let lo = (input[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(input[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Build a `ResolvedWatch` through the REAL 04-01 pipeline (FileConfig →
/// resolve()) so the client tests exercise config end-to-end.
pub fn watch_for(url: &str, username: Option<&str>, password: Option<&str>) -> ResolvedWatch {
    let tmp = std::env::temp_dir().join(format!("saussunet-watch-{}", std::process::id()));
    let mut toml = format!(
        "[storage]\ndata_dir = \"{}\"\n\n[vault]\nid = 1\nname = \"main\"\n\n[[watch]]\nurl = \"{url}\"\ninterval_secs = 300\n",
        tmp.display()
    );
    if let Some(u) = username {
        toml.push_str(&format!("username = \"{u}\"\n"));
    }
    if let Some(p) = password {
        toml.push_str(&format!("password = \"{p}\"\n"));
    }
    let cfg: FileConfig = toml::from_str(&toml).expect("watch config parses");
    cfg.resolve()
        .expect("resolve")
        .watch
        .into_iter()
        .next()
        .expect("one watch entry")
}

// ---------------------------------------------------------------------------
// Task 1 — DavClient unit-style tests against the stub
// ---------------------------------------------------------------------------

#[tokio::test]
async fn propfind_lists_children() {
    let (base, stub) = stub_webdav();
    stub.insert("note.md", b"hello note", "\"abc123\"");
    stub.insert("papers/", b"", "");

    let src = watch_for(&base, None, None);
    let client = DavClient::new(&src).expect("client builds");
    let entries = client.propfind_children("").await.expect("207 parsed");

    // Collection-self must be skipped by the parser.
    assert!(
        !entries.iter().any(|e| e.path.is_empty()),
        "self skipped: {entries:?}"
    );
    let note = entries
        .iter()
        .find(|e| e.path == "note.md")
        .unwrap_or_else(|| panic!("note.md missing: {entries:?}"));
    assert_eq!(note.etag.as_deref(), Some("\"abc123\""));
    assert_eq!(note.size, Some(10));
    assert!(!note.is_collection);
    let papers = entries
        .iter()
        .find(|e| e.path == "papers")
        .unwrap_or_else(|| panic!("papers missing: {entries:?}"));
    assert!(papers.is_collection);
}

#[tokio::test]
async fn get_bytes_respects_size_cap() {
    let (base, stub) = stub_webdav();
    stub.insert("big.md", b"0123456789abcdef", "\"big\"");

    let src = watch_for(&base, None, None);
    let client = DavClient::new(&src).expect("client builds");
    let err = client
        .get_bytes("big.md", 4)
        .await
        .expect_err("4-byte cap must refuse the 16-byte body");
    assert!(err.to_string().contains("4"), "cap in error: {err}");
    // Under the cap: exact bytes.
    let ok = client.get_bytes("big.md", 1024).await.expect("under cap");
    assert_eq!(ok, b"0123456789abcdef");
}

/// HIGH threat model: credentials ride ONLY on request headers. A failed
/// request's Display must carry neither the password nor Basic material.
#[tokio::test]
async fn error_string_has_no_password() {
    // Bind then drop a listener: the port is provably closed.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let src = watch_for(
        &format!("http://127.0.0.1:{port}/dav/"),
        Some("user"),
        Some("s3cr3t-stub"),
    );
    let client = DavClient::new(&src).expect("client builds");
    let err = client
        .propfind_children("")
        .await
        .expect_err("closed port must fail");
    let display = err.to_string();
    assert!(
        !display.contains("s3cr3t-stub"),
        "password leaked into error: {display}"
    );
    assert!(
        !display.contains("Basic"),
        "auth header material leaked: {display}"
    );
    // get_bytes shares the hygiene rule.
    let err2 = client
        .get_bytes("x.md", 1024)
        .await
        .expect_err("closed port must fail");
    assert!(!err2.to_string().contains("s3cr3t-stub"), "{err2}");
}

#[tokio::test]
async fn href_with_spaces_roundtrips() {
    let (base, stub) = stub_webdav();
    stub.insert("a b.md", b"spaces work", "\"sp\"");

    let src = watch_for(&base, None, None);
    let client = DavClient::new(&src).expect("client builds");
    let entries = client.propfind_children("").await.expect("207");
    let spaced = entries
        .iter()
        .find(|e| e.path == "a b.md")
        .unwrap_or_else(|| panic!("decoded href expected: {entries:?}"));
    assert_eq!(spaced.etag.as_deref(), Some("\"sp\""));
    // get_bytes must percent-encode the segment when rebuilding the URL.
    let bytes = client
        .get_bytes(&spaced.path, 1024)
        .await
        .expect("get_bytes on 'a b.md'");
    assert_eq!(bytes, b"spaces work");
}

// ---------------------------------------------------------------------------
// Task 2 — poll_once integration (T1, T11a): stub walk → diff → download →
// raw/watch copy → durable job. Zero real LLM (jobs are only ENQUEUED here).
// ---------------------------------------------------------------------------

use saussunet_core::pipeline::JOB_KIND_INGEST_RAW;
use saussunet_core::vault::Vault;
use saussunet_core::watch::poll_once;
use saussunet_core::{boot_open, Store};
use tempfile::TempDir;

/// Boot a real vault + store (04-01 config path end-to-end) pointed at the stub.
async fn setup_watch(stub_base: &str) -> (TempDir, i64, Arc<Store>, Arc<Vault>, ResolvedWatch) {
    let tmp = TempDir::new().expect("tempdir");
    let toml = format!(
        "[storage]\ndata_dir = \"{}\"\n\n[vault]\nid = 1\nname = \"main\"\n\n[[watch]]\nurl = \"{stub_base}\"\ninterval_secs = 300\n",
        tmp.path().display()
    );
    let cfg: FileConfig = toml::from_str(&toml).expect("config parses");
    let rt = cfg.resolve().expect("resolve");
    let (store, vault) = boot_open(&rt).await.expect("boot");
    let src = rt.watch.first().expect("one watch").clone();
    (tmp, vault.id, Arc::new(store), Arc::new(vault), src)
}

/// T1: one poll over a stub tree leaves pending `ingest_raw` jobs whose
/// payloads point at real `raw/watch/…` copies with provenance frontmatter,
/// plus watch_state rows (files + the directory bookkeeping row).
#[tokio::test]
async fn new_file_enqueues_ingest_job() {
    let (base, stub) = stub_webdav();
    stub.insert("note.md", b"# Note\nhello", "\"n1\"");
    stub.insert("papers/", b"", "");
    stub.insert("a b.md", b"spaces", "\"s1\"");
    let (_tmp, vault_id, store, vault, src) = setup_watch(&base).await;

    let enqueued = poll_once(&store, &vault, &src, 1 << 20)
        .await
        .expect("poll_once");
    assert_eq!(
        enqueued, 2,
        "note.md + 'a b.md' enqueue, papers/ never does"
    );

    let jobs = store.list_jobs(vault_id, Some("pending")).await.unwrap();
    assert_eq!(jobs.len(), 2);
    assert!(
        jobs.iter().all(|j| j.kind == JOB_KIND_INGEST_RAW),
        "md files route to ingest_raw: {:?}",
        jobs.iter().map(|j| &j.kind).collect::<Vec<_>>()
    );
    let mut filenames = Vec::new();
    for j in &jobs {
        let payload: serde_json::Value = serde_json::from_str(&j.payload_json).unwrap();
        let raw_path = payload["raw_path"].as_str().unwrap();
        assert!(
            raw_path.starts_with("raw/watch/"),
            "raw copy under raw/watch/: {raw_path}"
        );
        filenames.push(payload["filename"].as_str().unwrap().to_string());
        let on_disk = std::fs::read(vault.root.join(raw_path)).expect("raw copy on disk");
        let text = String::from_utf8_lossy(&on_disk);
        assert!(
            text.contains("source_url:"),
            "provenance frontmatter: {text}"
        );
        assert!(text.contains("sha256:"), "provenance frontmatter: {text}");
    }
    assert!(filenames.contains(&"note.md".to_string()));
    assert!(filenames.contains(&"a b.md".to_string()));

    // Second poll: identical etags → zero deltas → zero new jobs.
    assert_eq!(
        poll_once(&store, &vault, &src, 1 << 20)
            .await
            .expect("second poll"),
        0
    );
    assert_eq!(store.list_jobs(vault_id, None).await.unwrap().len(), 2);

    // watch_state: two file rows + the papers/ directory row (is_dir).
    let rows = store
        .list_watch_rows(vault_id, &src.source_id)
        .await
        .unwrap();
    let find = |p: &str| rows.iter().find(|r| r.path == p);
    assert!(find("note.md").is_some_and(|r| !r.is_dir && r.sha256.is_some()));
    assert!(find("a b.md").is_some_and(|r| !r.is_dir));
    assert!(find("papers").is_some_and(|r| r.is_dir));

    // record_sync_poll ran on the successful poll.
    let sync = store
        .get_sync_state(vault_id, &src.source_id)
        .await
        .unwrap();
    assert!(sync.is_some_and(|s| s.last_poll_at.is_some()));
}

/// T11a: .md/.txt → ingest_raw, .pdf → ingest_pdf, .docx → no job.
#[tokio::test]
async fn extension_allowlist_routing() {
    let (base, stub) = stub_webdav();
    stub.insert("a.md", b"md bytes", "\"1\"");
    stub.insert("b.txt", b"txt bytes", "\"2\"");
    stub.insert("c.pdf", b"%PDF-1.4\n% stub\n", "\"3\"");
    stub.insert("d.docx", b"docx bytes", "\"4\"");
    let (_tmp, vault_id, store, vault, src) = setup_watch(&base).await;

    let enqueued = poll_once(&store, &vault, &src, 1 << 20)
        .await
        .expect("poll_once");
    assert_eq!(enqueued, 3, "docx gets no job");

    let jobs = store.list_jobs(vault_id, Some("pending")).await.unwrap();
    let raws = jobs
        .iter()
        .filter(|j| j.kind == JOB_KIND_INGEST_RAW)
        .count();
    let pdfs = jobs
        .iter()
        .filter(|j| j.kind == saussunet_core::pipeline::JOB_KIND_INGEST_PDF)
        .count();
    assert_eq!(raws, 2, "a.md + b.txt → ingest_raw");
    assert_eq!(pdfs, 1, "c.pdf → ingest_pdf");
    assert!(
        !jobs.iter().any(|j| j.payload_json.contains("docx")),
        "d.docx must produce no job: {:?}",
        jobs.iter().map(|j| &j.payload_json).collect::<Vec<_>>()
    );
}
