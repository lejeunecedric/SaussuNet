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

/// One recorded request: (method, decoded path, Depth header, body).
pub type ReqLog = Vec<(String, String, Option<String>, String)>;

/// Handle into the running stub. Tree keys are DECODED vault-relative paths;
/// directory keys end with '/'.
#[derive(Clone)]
pub struct StubHandle {
    tree: Arc<Mutex<HashMap<String, StubFile>>>,
    log: Arc<Mutex<ReqLog>>,
    /// 04-07 knob (unused in 04-04): flip to make POST answer 207 instead of
    /// the D-05 fallback 400.
    sync_mode: Arc<AtomicBool>,
    /// 04-05 knob: when set, the NEXT PROPFIND answers 500 once (T7 walk
    /// failure), then the flag clears itself.
    fail_propfind: Arc<AtomicBool>,
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
    pub fn requests(&self) -> ReqLog {
        self.log.lock().unwrap().clone()
    }
    #[allow(dead_code)]
    pub fn set_sync_mode(&self, on: bool) {
        self.sync_mode.store(on, Ordering::SeqCst);
    }
    /// Make the next PROPFIND fail with 500 (one-shot).
    pub fn fail_next_propfind(&self) {
        self.fail_propfind.store(true, Ordering::SeqCst);
    }
}

/// Start the stub; returns its base URL (`http://127.0.0.1:<port>/`) + handle.
pub fn stub_webdav() -> (String, StubHandle) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("stub binds");
    let addr = listener.local_addr().expect("stub addr");
    let tree: Arc<Mutex<HashMap<String, StubFile>>> = Arc::new(Mutex::new(HashMap::new()));
    let log: Arc<Mutex<ReqLog>> = Arc::new(Mutex::new(Vec::new()));
    let sync_mode = Arc::new(AtomicBool::new(false));
    let fail_propfind = Arc::new(AtomicBool::new(false));
    let (tree_t, log_t, sync_t, fail_t) = (
        Arc::clone(&tree),
        Arc::clone(&log),
        Arc::clone(&sync_mode),
        Arc::clone(&fail_propfind),
    );
    std::thread::spawn(move || loop {
        let Ok((mut stream, _)) = listener.accept() else {
            break;
        };
        let tree = Arc::clone(&tree_t);
        let log = Arc::clone(&log_t);
        let sync_mode = Arc::clone(&sync_t);
        let fail_propfind = Arc::clone(&fail_t);
        std::thread::spawn(move || {
            let _ = handle_conn(&mut stream, &tree, &log, &sync_mode, &fail_propfind);
        });
    });
    (
        format!("http://{addr}/"),
        StubHandle {
            tree,
            log,
            sync_mode,
            fail_propfind,
        },
    )
}

fn handle_conn(
    stream: &mut std::net::TcpStream,
    tree: &Mutex<HashMap<String, StubFile>>,
    log: &Mutex<ReqLog>,
    sync_mode: &AtomicBool,
    fail_propfind: &AtomicBool,
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
        "PROPFIND" => {
            if fail_propfind.swap(false, Ordering::SeqCst) {
                // 04-05 T7: one-shot walk failure (DavClient requires 207).
                (500, vec![], b"boom".to_vec())
            } else {
                propfind_response(tree, &raw_path, &path)
            }
        }
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
        500 => "Internal Server Error",
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
    // 04-05: a collection is walkable if it has an explicit dir key OR any
    // deeper key under it (`a/b/c.md` makes `a/` and `a/b/` walkable without
    // explicit dir keys — implicit collections are synthesized below).
    if !key.is_empty() && !guard.keys().any(|k| k.starts_with(&key)) {
        return (404, vec![], b"Not Found".to_vec());
    }
    // One-level children of `key`: explicit file/dir keys PLUS implicit
    // collections synthesized from deeper keys, sorted for determinism.
    let mut children: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for k in guard.keys() {
        let Some(rest) = k.strip_prefix(&key) else {
            continue;
        };
        if rest.is_empty() {
            continue;
        }
        match rest.find('/') {
            // A deeper segment ⇒ the first segment names a (possibly
            // implicit) collection at this level.
            Some(idx) => {
                children.insert(format!("{key}{}/", &rest[..idx]));
            }
            // A file or explicit dir key sitting directly at this level.
            None => {
                children.insert(k.clone());
            }
        }
    }
    let children: Vec<String> = children.into_iter().collect();

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
        if child.ends_with('/') {
            xml.push_str(&format!(
                "  <d:response><d:href>{href}</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>\n"
            ));
        } else {
            let entry = guard.get(&child).expect("key just listed");
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
    setup_watch_with(stub_base, None, None, None).await
}

/// 04-05 variant: same boot, with optional `depth_cap` / basic-auth overrides.
async fn setup_watch_with(
    stub_base: &str,
    depth_cap: Option<u32>,
    username: Option<&str>,
    password: Option<&str>,
) -> (TempDir, i64, Arc<Store>, Arc<Vault>, ResolvedWatch) {
    let tmp = TempDir::new().expect("tempdir");
    let mut toml = format!(
        "[storage]\ndata_dir = \"{}\"\n\n[vault]\nid = 1\nname = \"main\"\n\n[[watch]]\nurl = \"{stub_base}\"\ninterval_secs = 300\n",
        tmp.path().display()
    );
    if let Some(d) = depth_cap {
        toml.push_str(&format!("depth_cap = {d}\n"));
    }
    if let Some(u) = username {
        toml.push_str(&format!("username = \"{u}\"\n"));
    }
    if let Some(p) = password {
        toml.push_str(&format!("password = \"{p}\"\n"));
    }
    let cfg: FileConfig = toml::from_str(&toml).expect("config parses");
    let rt = cfg.resolve().expect("resolve");
    let (store, vault) = boot_open(&rt).await.expect("boot");
    let src = rt.watch.first().expect("one watch").clone();
    (tmp, vault.id, Arc::new(store), Arc::new(vault), src)
}

/// Byte-snapshot of every file under `root` (sorted) — used to prove removals
/// never touch vault pages (D-09).
fn snapshot_dir(root: &std::path::Path) -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<(String, Vec<u8>)> = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let rel = p.strip_prefix(root).unwrap().display().to_string();
                out.push((rel, std::fs::read(&p).unwrap()));
            }
        }
    }
    out.sort();
    out
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

// ---------------------------------------------------------------------------
// Task 1 (04-05) — full BFS walk under the depth cap, removals, skip notes
// (T3, T4, T11b + log-injection defense).
// ---------------------------------------------------------------------------

/// T4 (SRCC-01): BFS walk sends Depth: 1 on EVERY PROPFIND and never
/// descends past `depth_cap`. A shallow source misses the deep file; a
/// second source with the default cap finds it.
#[tokio::test]
async fn walk_depth_cap() {
    let (base, stub) = stub_webdav();
    stub.insert("top.md", b"top", "\"t\"");
    // Implicit dirs: d1 (depth 1), d1/d2 (2), d1/d2/d3 (3); deep.md at depth 4.
    stub.insert("d1/d2/d3/deep.md", b"deep", "\"d\"");

    // Source A: depth_cap = 2 ⇒ PROPFINDs at depths 0..2 only.
    let (_tmp, vault_id, store, vault, src) = setup_watch_with(&base, Some(2), None, None).await;
    assert_eq!(src.depth_cap, 2);
    let enqueued = poll_once(&store, &vault, &src, 1 << 20)
        .await
        .expect("poll cap=2");
    assert_eq!(enqueued, 1, "only top.md is visible at depth_cap=2");

    let reqs = stub.requests();
    let propfinds: Vec<&(String, String, Option<String>, String)> =
        reqs.iter().filter(|(m, ..)| m == "PROPFIND").collect();
    assert!(
        propfinds.len() >= 3,
        "root + d1 + d1/d2 walked: {propfinds:?}"
    );
    for (_, _, depth, _) in &propfinds {
        assert_eq!(
            depth.as_deref(),
            Some("1"),
            "SRCC-01: every PROPFIND must carry Depth: 1 (never infinity)"
        );
    }
    for (_, path, _, _) in &propfinds {
        let levels = path
            .trim_matches('/')
            .split('/')
            .filter(|s| !s.is_empty())
            .count();
        assert!(
            levels <= 2,
            "no PROPFIND may target a path deeper than the cap: {path}"
        );
        assert!(
            !path.contains("d3"),
            "walk must stop before d3 at depth_cap=2: {path}"
        );
    }
    let jobs = store.list_jobs(vault_id, None).await.unwrap();
    assert_eq!(jobs.len(), 1);
    assert!(
        !jobs.iter().any(|j| j.payload_json.contains("deep")),
        "deep.md must NOT be enqueued at depth_cap=2"
    );

    // Source B: depth_cap = 4 (fresh vault, same stub) ⇒ deep.md is found.
    let (_tmp2, vid2, store2, vault2, src2) = setup_watch_with(&base, Some(4), None, None).await;
    let enq2 = poll_once(&store2, &vault2, &src2, 1 << 20)
        .await
        .expect("poll cap=4");
    assert_eq!(enq2, 2, "top.md + deep.md at depth_cap=4");
    let rows = store2.list_watch_rows(vid2, &src2.source_id).await.unwrap();
    assert!(rows.iter().any(|r| r.path == "d1/d2/d3/deep.md"));
    assert!(rows.iter().any(|r| r.path == "d1" && r.is_dir));
}

/// T3 (D-09): a file deleted server-side yields a log.md removal entry and a
/// deleted watch_state row — and NEVER touches vault pages.
#[tokio::test]
async fn removed_file_logs_and_prunes_state() {
    let (base, stub) = stub_webdav();
    stub.insert("keep.md", b"keep", "\"k\"");
    stub.insert("gone.md", b"gone", "\"g\"");
    let (_tmp, vault_id, store, vault, src) = setup_watch(&base).await;
    assert_eq!(
        poll_once(&store, &vault, &src, 1 << 20)
            .await
            .expect("first poll"),
        2
    );

    // A vault page that removals must never touch (D-09).
    let pages = vault.root.join("pages");
    std::fs::create_dir_all(&pages).expect("pages dir");
    std::fs::write(pages.join("test-page.md"), b"# Page\nuntouchable\n").unwrap();
    let pages_before = snapshot_dir(&pages);
    let jobs_before = store.list_jobs(vault_id, None).await.unwrap().len();

    stub.remove("gone.md");
    assert_eq!(
        poll_once(&store, &vault, &src, 1 << 20)
            .await
            .expect("poll after removal"),
        0,
        "a removal never enqueues a job"
    );

    let log = std::fs::read_to_string(vault.root.join("log.md")).expect("log.md");
    assert!(log.contains("removed: gone.md"), "removal logged: {log}");
    assert!(!log.contains("removed: keep.md"), "keep.md must survive");

    let rows = store
        .list_watch_rows(vault_id, &src.source_id)
        .await
        .unwrap();
    assert!(
        !rows.iter().any(|r| r.path == "gone.md"),
        "state row pruned"
    );
    assert!(
        rows.iter().any(|r| r.path == "keep.md"),
        "keep.md row intact"
    );

    assert_eq!(snapshot_dir(&pages), pages_before, "vault pages untouched");
    assert_eq!(
        store.list_jobs(vault_id, None).await.unwrap().len(),
        jobs_before
    );
}

/// T11b (D-08): a non-allowlisted extension is logged ONCE across polls —
/// the marker state row is the "already noted" memory — and gets no job.
#[tokio::test]
async fn skipped_ext_logged_once_not_every_poll() {
    let (base, stub) = stub_webdav();
    stub.insert("a.md", b"md", "\"1\"");
    stub.insert("d.docx", b"docx bytes", "\"4\"");
    let (_tmp, vault_id, store, vault, src) = setup_watch(&base).await;

    assert_eq!(
        poll_once(&store, &vault, &src, 1 << 20)
            .await
            .expect("first poll"),
        1,
        "docx gets no job"
    );
    assert_eq!(
        poll_once(&store, &vault, &src, 1 << 20)
            .await
            .expect("second poll"),
        0
    );

    let log = std::fs::read_to_string(vault.root.join("log.md")).expect("log.md");
    let skip_lines = log.lines().filter(|l| l.contains("skipped")).count();
    assert_eq!(skip_lines, 1, "skip note logged exactly once: {log}");
    assert!(log.contains("skipped (extension not allowlisted): d.docx"));
    assert_eq!(store.list_jobs(vault_id, None).await.unwrap().len(), 1);

    // The marker row exists with sha256 NULL (it is the already-noted marker).
    let rows = store
        .list_watch_rows(vault_id, &src.source_id)
        .await
        .unwrap();
    let docx = rows
        .iter()
        .find(|r| r.path == "d.docx")
        .expect("marker row");
    assert!(docx.sha256.is_none(), "skip marker carries no sha256");
}

/// Threat model (log injection): a crafted filename carrying a newline must
/// not forge an extra log.md entry header.
#[tokio::test]
async fn log_injection_via_filename_is_neutralized() {
    let (base, stub) = stub_webdav();
    stub.insert("evil\nINJECTED.md", b"x", "\"e\"");
    let (_tmp, _vault_id, store, vault, src) = setup_watch(&base).await;
    let before = std::fs::read_to_string(vault.root.join("log.md")).unwrap_or_default();
    let base_count = saussunet_core::vault::log::entry_count(&before);

    assert_eq!(
        poll_once(&store, &vault, &src, 1 << 20)
            .await
            .expect("poll_once"),
        1
    );

    let log = std::fs::read_to_string(vault.root.join("log.md")).expect("log.md");
    assert!(
        !log.contains("\nINJECTED"),
        "newline from an untrusted filename forged a log line: {log}"
    );
    assert_eq!(
        saussunet_core::vault::log::entry_count(&log),
        base_count + 1,
        "exactly one Watch entry, no forged header"
    );
}

// ---------------------------------------------------------------------------
// Task 2 (04-05) — sha256 gate, failure bookkeeping, silent no-op
// (T2, T7, T9, T10).
// ---------------------------------------------------------------------------

/// T2: changing bytes + etag on the stub re-enqueues the file and the log
/// records a `changed:` line.
#[tokio::test]
async fn changed_file_reenqueues() {
    let (base, stub) = stub_webdav();
    stub.insert("note.md", b"v1 bytes", "\"e1\"");
    let (_tmp, vault_id, store, vault, src) = setup_watch(&base).await;
    assert_eq!(
        poll_once(&store, &vault, &src, 1 << 20)
            .await
            .expect("first poll"),
        1
    );

    stub.insert("note.md", b"v2 bytes differ", "\"e2\"");
    assert_eq!(
        poll_once(&store, &vault, &src, 1 << 20)
            .await
            .expect("poll after change"),
        1,
        "changed bytes must re-enqueue"
    );
    assert_eq!(store.list_jobs(vault_id, None).await.unwrap().len(), 2);
    let log = std::fs::read_to_string(vault.root.join("log.md")).expect("log.md");
    assert!(
        log.contains("changed: note.md"),
        "changed line logged: {log}"
    );
}

/// T10 (D-07): rotating ONLY the etag (same bytes) enqueues nothing — the
/// sha256 gate absorbs weak-etag churn — but the state row picks up the new
/// etag with the sha256 unchanged.
#[tokio::test]
async fn weak_etag_same_bytes_no_enqueue() {
    let (base, stub) = stub_webdav();
    stub.insert("note.md", b"stable bytes", "\"e1\"");
    let (_tmp, vault_id, store, vault, src) = setup_watch(&base).await;
    assert_eq!(
        poll_once(&store, &vault, &src, 1 << 20)
            .await
            .expect("first poll"),
        1
    );
    let sha_before = store
        .list_watch_rows(vault_id, &src.source_id)
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.path == "note.md")
        .and_then(|r| r.sha256)
        .expect("sha256 after first poll");

    stub.set_etag("note.md", "W/\"e2\"");
    assert_eq!(
        poll_once(&store, &vault, &src, 1 << 20)
            .await
            .expect("poll after etag rotation"),
        0,
        "same bytes under a rotated etag must not enqueue"
    );
    assert_eq!(store.list_jobs(vault_id, None).await.unwrap().len(), 1);
    let row = store
        .list_watch_rows(vault_id, &src.source_id)
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.path == "note.md")
        .expect("row");
    assert_eq!(row.etag.as_deref(), Some("W/\"e2\""), "fresh etag recorded");
    assert_eq!(
        row.sha256.as_deref(),
        Some(sha_before.as_str()),
        "sha256 unchanged"
    );
}

/// T9 (D-11): re-polling a static tree is a NO-OP — zero jobs AND log.md
/// byte-identical between polls (silent, DEBUG tracing only).
#[tokio::test]
async fn unchanged_poll_is_noop() {
    let (base, stub) = stub_webdav();
    stub.insert("a.md", b"a", "\"1\"");
    stub.insert("papers/", b"", "");
    let (_tmp, vault_id, store, vault, src) = setup_watch(&base).await;
    assert_eq!(
        poll_once(&store, &vault, &src, 1 << 20)
            .await
            .expect("first poll"),
        1
    );
    let log_before = std::fs::read(vault.root.join("log.md")).expect("log.md after poll 1");
    let jobs_before = store.list_jobs(vault_id, None).await.unwrap().len();

    assert_eq!(
        poll_once(&store, &vault, &src, 1 << 20)
            .await
            .expect("second poll"),
        0
    );
    assert_eq!(
        store.list_jobs(vault_id, None).await.unwrap().len(),
        jobs_before
    );
    assert_eq!(
        std::fs::read(vault.root.join("log.md")).expect("log.md"),
        log_before,
        "no-op poll must not touch log.md"
    );
}

/// T7a: a successful walk advances `last_poll_at` and keeps `last_error`
/// clear.
#[tokio::test]
async fn sync_state_updated_after_walk() {
    let (base, stub) = stub_webdav();
    stub.insert("a.md", b"a", "\"1\"");
    let (_tmp, vault_id, store, vault, src) = setup_watch(&base).await;
    poll_once(&store, &vault, &src, 1 << 20)
        .await
        .expect("poll 1");
    let s1 = store
        .get_sync_state(vault_id, &src.source_id)
        .await
        .unwrap()
        .expect("sync row");
    assert!(s1.last_poll_at.is_some(), "last_poll_at set after success");
    assert!(s1.last_error.is_none(), "clean poll has no last_error");

    poll_once(&store, &vault, &src, 1 << 20)
        .await
        .expect("poll 2");
    let s2 = store
        .get_sync_state(vault_id, &src.source_id)
        .await
        .unwrap()
        .expect("sync row");
    assert!(
        s2.last_poll_at.as_deref() > s1.last_poll_at.as_deref(),
        "last_poll_at advanced: {:?} -> {:?}",
        s1.last_poll_at,
        s2.last_poll_at
    );
    assert!(s2.last_error.is_none());
}

/// T7b: a failed walk records a sanitized `last_error` (NEVER the stub
/// password), applies ZERO removals (Pitfall 3), leaves state and log.md
/// untouched, and the next tick recovers cleanly.
#[tokio::test]
async fn poll_failure_records_last_error() {
    let (base, stub) = stub_webdav();
    stub.insert("note.md", b"hello", "\"n1\"");
    let (_tmp, vault_id, store, vault, src) =
        setup_watch_with(&base, None, Some("user"), Some("s3cr3t-stub-pw")).await;
    assert_eq!(
        poll_once(&store, &vault, &src, 1 << 20)
            .await
            .expect("clean poll"),
        1
    );
    let rows_before = store
        .list_watch_rows(vault_id, &src.source_id)
        .await
        .unwrap()
        .len();
    let log_before = std::fs::read(vault.root.join("log.md")).expect("log.md");

    stub.fail_next_propfind();
    poll_once(&store, &vault, &src, 1 << 20)
        .await
        .expect_err("forced PROPFIND failure must surface as Err");

    let sync = store
        .get_sync_state(vault_id, &src.source_id)
        .await
        .unwrap()
        .expect("sync row");
    let err = sync.last_error.expect("last_error recorded");
    assert!(
        !err.contains("s3cr3t-stub-pw"),
        "credential leaked into last_error: {err}"
    );
    // Pitfall 3: a failed walk must prune NOTHING and log NOTHING.
    assert_eq!(
        store
            .list_watch_rows(vault_id, &src.source_id)
            .await
            .unwrap()
            .len(),
        rows_before,
        "zero state rows deleted on a failed walk"
    );
    assert_eq!(
        std::fs::read(vault.root.join("log.md")).expect("log.md"),
        log_before,
        "failed walk writes no log lines"
    );

    // Next tick recovers: clean walk clears last_error.
    assert_eq!(
        poll_once(&store, &vault, &src, 1 << 20)
            .await
            .expect("recovery poll"),
        0
    );
    let sync = store
        .get_sync_state(vault_id, &src.source_id)
        .await
        .unwrap()
        .expect("sync row");
    assert!(
        sync.last_error.is_none(),
        "last_error cleared after recovery"
    );
}

// ---------------------------------------------------------------------------
// 04-06 — daemon lifecycle: interval loop (T5) + restart-resume proof (T6)
// ---------------------------------------------------------------------------

/// T5 (D-10): `spawn_watcher` polls immediately on spawn and repeats on the
/// per-source interval. 10 ms interval ⇒ ≥2 stub requests within ~150 ms,
/// yet exactly ONE ingest job total (watch_state dedups across ticks), and
/// aborting the handle stops all further requests.
#[tokio::test]
async fn interval_loop_polls_on_spawn_and_repeats() {
    use saussunet_core::watch::spawn_watcher;

    let (base, stub) = stub_webdav();
    stub.insert("note.md", b"hello", "\"n1\"");
    let (_tmp, vault_id, store, vault, mut src) = setup_watch(&base).await;
    src.interval = std::time::Duration::from_millis(10);

    let handle = spawn_watcher(store.clone(), vault.clone(), src.clone(), 1 << 20);
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    assert!(
        stub.requests().len() >= 2,
        "interval loop must poll repeatedly: {:?}",
        stub.requests()
    );
    assert_eq!(
        store.list_jobs(vault_id, None).await.unwrap().len(),
        1,
        "dedup via watch_state across ticks — one job total"
    );

    handle.abort();
    let after_abort = stub.requests().len();
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    assert_eq!(
        stub.requests().len(),
        after_abort,
        "aborted watcher must make no further requests"
    );
}
/// T6 (SRCC-02 headline): a FULL restart on a FILE-BACKED SQLite DB resumes
/// watching with zero re-scan. poll → drop(store) → reopen the same path →
/// poll again ⇒ zero new jobs and `last_poll_at` advanced; then a genuinely
/// new file is still picked up (resume is live, not frozen). `boot_open` on a
/// tempdir `data_dir` is the production sequence — the DB is a real file, so
/// a pass cannot be faked by an in-memory store.
#[tokio::test]
async fn restart_resumes_without_rescan() {
    let (base, stub) = stub_webdav();
    stub.insert("note.md", b"hello", "\"n1\"");
    let tmp = TempDir::new().expect("tempdir");
    let toml = format!(
        "[storage]\ndata_dir = \"{}\"\n\n[vault]\nid = 1\nname = \"main\"\n\n[[watch]]\nurl = \"{base}\"\ninterval_secs = 300\n",
        tmp.path().display()
    );
    let cfg: FileConfig = toml::from_str(&toml).expect("config parses");
    let rt = cfg.resolve().expect("resolve");
    assert!(
        rt.db_path.starts_with(tmp.path()),
        "T6 requires a file-backed DB, got {}",
        rt.db_path.display()
    );
    let src = rt.watch.first().expect("one watch").clone();

    // --- boot #1: first poll enqueues the one file ---
    let (store, vault) = boot_open(&rt).await.expect("boot 1");
    let (store, vault) = (Arc::new(store), Arc::new(vault));
    assert_eq!(
        poll_once(&store, &vault, &src, 1 << 20)
            .await
            .expect("first poll"),
        1
    );
    let jobs_before = store.list_jobs(1, None).await.unwrap().len();
    let sync_before = store
        .get_sync_state(1, &src.source_id)
        .await
        .unwrap()
        .expect("sync row")
        .last_poll_at
        .expect("last_poll_at after first poll");

    // --- full process restart: everything dropped, same paths reopened ---
    drop(store);
    drop(vault);
    let (store, vault) = boot_open(&rt).await.expect("boot 2 (restart)");
    let (store, vault) = (Arc::new(store), Arc::new(vault));

    assert_eq!(
        poll_once(&store, &vault, &src, 1 << 20)
            .await
            .expect("poll after restart"),
        0,
        "restart must NOT re-scan unchanged files (SRCC-02)"
    );
    assert_eq!(
        store.list_jobs(1, None).await.unwrap().len(),
        jobs_before,
        "zero new jobs across the restart"
    );
    let sync_after = store
        .get_sync_state(1, &src.source_id)
        .await
        .unwrap()
        .expect("sync row")
        .last_poll_at
        .expect("last_poll_at after restart poll");
    assert!(
        sync_after > sync_before,
        "last_poll_at must advance across restart: {sync_before} -> {sync_after}"
    );

    // Resume is LIVE, not frozen: a new file after restart enqueues exactly 1.
    stub.insert("later.md", b"later", "\"l1\"");
    assert_eq!(
        poll_once(&store, &vault, &src, 1 << 20)
            .await
            .expect("post-restart poll"),
        1
    );
    assert_eq!(
        store.list_jobs(1, None).await.unwrap().len(),
        jobs_before + 1
    );
}
