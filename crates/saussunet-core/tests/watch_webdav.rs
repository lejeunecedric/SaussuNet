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
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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

/// Per-item status in a scripted sync reply (RFC 6578: 200 = added/changed,
/// 404 = removed).
#[derive(Clone, Copy, Debug)]
pub enum ItemStatus {
    Ok,
    Removed,
}

/// One scripted sync-collection 207 body: items + trailing sync-token.
#[derive(Clone, Debug)]
pub struct SyncReply {
    pub items: Vec<(String, ItemStatus)>,
    pub token: String,
}

/// 04-07 sync stub modes (plan 04-07 Context).
#[derive(Clone, Debug)]
pub enum SyncMode {
    /// Default: every POST answers 400 (server has no sync-collection).
    Unsupported,
    /// Answer POSTs with scripted 207 replies in order (last reply repeats).
    Enabled { script: Vec<SyncReply> },
}

/// Shared sync state for the stub's POST handler.
#[derive(Clone)]
struct StubSync {
    mode: Arc<Mutex<SyncMode>>,
    /// When set, a POST whose body contains this string answers 400
    /// (stale-token simulation, Pitfall 5).
    reject_token: Arc<Mutex<Option<String>>>,
    /// Counts POSTs so scripted replies advance one per request.
    posts: Arc<AtomicUsize>,
}

/// Handle into the running stub. Tree keys are DECODED vault-relative paths;
/// directory keys end with '/'.
#[derive(Clone)]
pub struct StubHandle {
    tree: Arc<Mutex<HashMap<String, StubFile>>>,
    log: Arc<Mutex<ReqLog>>,
    /// 04-07 knob: sync-collection behavior of the POST handler.
    sync: StubSync,
    /// 04-05 knob: when set, the NEXT PROPFIND answers 500 once (T7 walk
    /// failure), then the flag clears itself.
    fail_propfind: Arc<AtomicBool>,
    /// H1 regression knob: GETs whose decoded path is in this set answer 500
    /// PERSISTENTLY (a file that fails every tick — the source-wedging case).
    fail_gets: Arc<Mutex<Vec<String>>>,
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
    /// 04-07: script the sync-collection (POST) behavior.
    pub fn set_sync_mode(&self, mode: SyncMode) {
        *self.sync.mode.lock().unwrap() = mode;
    }
    /// 04-07: POSTs whose body contains `token` answer 400 (stale-token
    /// simulation — Pitfall 5 guard).
    pub fn set_sync_reject_token(&self, token: &str) {
        *self.sync.reject_token.lock().unwrap() = Some(token.to_string());
    }
    /// Make the next PROPFIND fail with 500 (one-shot).
    pub fn fail_next_propfind(&self) {
        self.fail_propfind.store(true, Ordering::SeqCst);
    }
    /// H1: every GET of `path` (tree key, no leading '/') answers 500 forever.
    pub fn fail_get(&self, path: &str) {
        self.fail_gets.lock().unwrap().push(path.to_string());
    }
}

/// Start the stub; returns its base URL (`http://127.0.0.1:<port>/`) + handle.
pub fn stub_webdav() -> (String, StubHandle) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("stub binds");
    let addr = listener.local_addr().expect("stub addr");
    let tree: Arc<Mutex<HashMap<String, StubFile>>> = Arc::new(Mutex::new(HashMap::new()));
    let log: Arc<Mutex<ReqLog>> = Arc::new(Mutex::new(Vec::new()));
    let sync = StubSync {
        mode: Arc::new(Mutex::new(SyncMode::Unsupported)),
        reject_token: Arc::new(Mutex::new(None)),
        posts: Arc::new(AtomicUsize::new(0)),
    };
    let fail_propfind = Arc::new(AtomicBool::new(false));
    let fail_gets: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let (tree_t, log_t, sync_t, fail_t, fail_gets_t) = (
        Arc::clone(&tree),
        Arc::clone(&log),
        sync.clone(),
        Arc::clone(&fail_propfind),
        Arc::clone(&fail_gets),
    );
    std::thread::spawn(move || loop {
        let Ok((mut stream, _)) = listener.accept() else {
            break;
        };
        let tree = Arc::clone(&tree_t);
        let log = Arc::clone(&log_t);
        let sync = sync_t.clone();
        let fail_propfind = Arc::clone(&fail_t);
        let fail_gets = Arc::clone(&fail_gets_t);
        std::thread::spawn(move || {
            let _ = handle_conn(&mut stream, &tree, &log, &sync, &fail_propfind, &fail_gets);
        });
    });
    (
        format!("http://{addr}/"),
        StubHandle {
            tree,
            log,
            sync,
            fail_propfind,
            fail_gets,
        },
    )
}

fn handle_conn(
    stream: &mut std::net::TcpStream,
    tree: &Mutex<HashMap<String, StubFile>>,
    log: &Mutex<ReqLog>,
    sync: &StubSync,
    fail_propfind: &AtomicBool,
    fail_gets: &Mutex<Vec<String>>,
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
        "GET" => {
            if fail_gets.lock().unwrap().iter().any(|p| p == path.trim_start_matches('/')) {
                // H1: persistent per-file GET failure (the wedging case).
                (500, vec![], b"boom".to_vec())
            } else {
                get_response(tree, &path)
            }
        }
        "POST" => {
            // RFC 6578 sync-collection (POST-as-REPORT). Bodies already
            // land in the request log — tests assert on recorded bodies.
            let idx = sync.posts.fetch_add(1, Ordering::SeqCst);
            let mode = sync.mode.lock().unwrap().clone();
            let reject = sync.reject_token.lock().unwrap().clone();
            let body_str = String::from_utf8_lossy(&body).to_string();
            match &mode {
                SyncMode::Unsupported => (400, vec![], b"Not Implemented".to_vec()),
                _ if reject.as_deref().is_some_and(|r| body_str.contains(r)) => {
                    (400, vec![], b"stale sync-token".to_vec())
                }
                SyncMode::Enabled { script } => {
                    let reply = script
                        .get(idx)
                        .or_else(|| script.last())
                        .expect("sync script non-empty");
                    sync_report_response(tree, &raw_path, reply)
                }
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

/// Build a scripted sync-report 207 body (SYNC207 fixture shape from
/// watch/multistatus.rs tests): per-item `<d:status>` 200/404 plus a
/// trailing `<d:sync-token>`. 200-items take etag/size from the tree when
/// present (so the poller's GET of a changed file succeeds).
fn sync_report_response(
    tree: &Mutex<HashMap<String, StubFile>>,
    raw_path: &str,
    reply: &SyncReply,
) -> (u16, Vec<String>, Vec<u8>) {
    let self_href = if raw_path.is_empty() { "/" } else { raw_path };
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<d:multistatus xmlns:d=\"DAV:\">\n",
    );
    for (path, status) in &reply.items {
        let href = format!(
            "{}/{}",
            self_href.trim_end_matches('/'),
            path.split('/')
                .map(encode_segment)
                .collect::<Vec<_>>()
                .join("/")
        );
        match status {
            ItemStatus::Ok => {
                let (etag, len) = tree
                    .lock()
                    .unwrap()
                    .get(path.as_str())
                    .map(|f| (f.etag.clone(), f.bytes.len()))
                    .unwrap_or_else(|| ("\"sync\"".to_string(), 0usize));
                xml.push_str(&format!(
                    "  <d:response><d:href>{href}</d:href><d:propstat><d:prop><d:getetag>{etag}</d:getetag><d:getcontentlength>{len}</d:getcontentlength></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat><d:status>HTTP/1.1 200 OK</d:status></d:response>\n"
                ));
            }
            ItemStatus::Removed => {
                xml.push_str(&format!(
                    "  <d:response><d:href>{href}</d:href><d:propstat><d:prop/><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat><d:status>HTTP/1.1 404 Not Found</d:status></d:response>\n"
                ));
            }
        }
    }
    xml.push_str(&format!("<d:sync-token>{}</d:sync-token>", reply.token));
    xml.push_str("</d:multistatus>");
    (
        207,
        vec!["Content-Type: application/xml".to_string()],
        xml.into_bytes(),
    )
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

// ---------------------------------------------------------------------------
// 04-07 — opportunistic RFC 6578 sync-collection (T8a/T8b + Pitfall 5)
// ---------------------------------------------------------------------------

/// T8a: first poll POSTs sync-collection with an EMPTY token, the returned
/// `<d:sync-token>` persists in sync_state.token, the second poll's report
/// body carries that token, and a 404-status item yields the removal log +
/// state prune + token advance. No PROPFIND is ever sent.
#[tokio::test]
async fn sync_token_persisted_and_reused() {
    let (base, stub) = stub_webdav();
    stub.insert("note.md", b"hello", "\"n1\"");
    stub.set_sync_mode(SyncMode::Enabled {
        script: vec![
            SyncReply {
                items: vec![("note.md".to_string(), ItemStatus::Ok)],
                token: "http://stub/sync/1".to_string(),
            },
            SyncReply {
                items: vec![("gone.md".to_string(), ItemStatus::Removed)],
                token: "http://stub/sync/2".to_string(),
            },
        ],
    });
    let (_tmp, vault_id, store, vault, src) = setup_watch(&base).await;

    // Poll 1: initial sync (empty token) → note.md enqueued, token persisted.
    assert_eq!(
        poll_once(&store, &vault, &src, 1 << 20)
            .await
            .expect("sync poll 1"),
        1
    );
    let sync = store
        .get_sync_state(vault_id, &src.source_id)
        .await
        .unwrap()
        .expect("sync row");
    assert_eq!(sync.token.as_deref(), Some("http://stub/sync/1"));
    assert!(!sync.sync_unsupported, "sync-capable server must not latch");
    assert_eq!(store.list_jobs(vault_id, None).await.unwrap().len(), 1);

    // Poll 2: carries token 1; 404-item → removal log + prune + advance.
    assert_eq!(
        poll_once(&store, &vault, &src, 1 << 20)
            .await
            .expect("sync poll 2"),
        0
    );
    let sync = store
        .get_sync_state(vault_id, &src.source_id)
        .await
        .unwrap()
        .expect("sync row");
    assert_eq!(sync.token.as_deref(), Some("http://stub/sync/2"));

    let log = std::fs::read_to_string(vault.root.join("log.md")).expect("log.md");
    assert!(log.contains("removed: gone.md"), "404-item logged: {log}");
    assert!(log.contains("added: note.md"), "200-item logged: {log}");
    let rows = store
        .list_watch_rows(vault_id, &src.source_id)
        .await
        .unwrap();
    assert!(!rows.iter().any(|r| r.path == "gone.md"), "row pruned");
    assert!(
        rows.iter().any(|r| r.path == "note.md"),
        "note.md row intact"
    );

    // Request-shape assertions (T8a): exactly two POSTs, zero PROPFINDs.
    let reqs = stub.requests();
    let posts: Vec<&(String, String, Option<String>, String)> =
        reqs.iter().filter(|(m, ..)| m == "POST").collect();
    assert_eq!(posts.len(), 2, "one sync report per poll: {reqs:?}");
    assert!(
        !reqs.iter().any(|(m, ..)| m == "PROPFIND"),
        "sync-capable poll must not walk: {reqs:?}"
    );
    for (_, _, depth, _) in &posts {
        assert_eq!(depth.as_deref(), Some("0"), "POST-as-REPORT is Depth: 0");
    }
    assert!(
        posts[0].3.contains("<d:sync-token></d:sync-token>"),
        "initial sync carries an empty token: {}",
        posts[0].3
    );
    assert!(
        posts[0].3.contains("<d:sync-level>infinity</d:sync-level>"),
        "sync-level infinity at the root (one token per source): {}",
        posts[0].3
    );
    assert!(
        posts[1].3.contains("http://stub/sync/1"),
        "second report carries the persisted token: {}",
        posts[1].3
    );
}

/// T8b: a server that 400s the report latches sync_unsupported after ONE
/// POST ever; every subsequent poll is PROPFIND-only and ingestion keeps
/// working through the walk.
#[tokio::test]
async fn sync_unsupported_falls_back_forever() {
    let (base, stub) = stub_webdav(); // default: SyncMode::Unsupported
    stub.insert("note.md", b"hello", "\"n1\"");
    let (_tmp, vault_id, store, vault, src) = setup_watch(&base).await;

    // Poll 1: POST 400 → latch → walk applies the delta in the SAME poll.
    assert_eq!(
        poll_once(&store, &vault, &src, 1 << 20)
            .await
            .expect("fallback poll 1"),
        1,
        "walk ingestion must still work on the latching poll"
    );
    let sync = store
        .get_sync_state(vault_id, &src.source_id)
        .await
        .unwrap()
        .expect("sync row");
    assert!(sync.sync_unsupported, "D-05 latch set after 400");

    // Poll 2: zero further POSTs ever — PROPFIND-only from here on.
    assert_eq!(
        poll_once(&store, &vault, &src, 1 << 20)
            .await
            .expect("fallback poll 2"),
        0
    );
    let posts = stub.requests().iter().filter(|(m, ..)| m == "POST").count();
    assert_eq!(posts, 1, "exactly one POST ever");

    // Ingestion via the walk still works after the latch.
    stub.insert("later.md", b"later", "\"l1\"");
    assert_eq!(
        poll_once(&store, &vault, &src, 1 << 20)
            .await
            .expect("fallback poll 3"),
        1
    );
    assert_eq!(
        stub.requests().iter().filter(|(m, ..)| m == "POST").count(),
        1,
        "still exactly one POST ever"
    );
}

/// Pitfall 5: a 400 on a STORED token triggers exactly one empty-token
/// retry (full re-sync) — never a latch, never fatal. The sha256 gate
/// absorbs the re-sync without a re-ingest storm.
#[tokio::test]
async fn stale_token_resyncs_not_latches() {
    let (base, stub) = stub_webdav();
    stub.insert("note.md", b"hello", "\"n1\"");
    stub.set_sync_mode(SyncMode::Enabled {
        script: vec![SyncReply {
            items: vec![("note.md".to_string(), ItemStatus::Ok)],
            token: "http://stub/sync/1".to_string(),
        }],
    });
    stub.set_sync_reject_token("stale-token");
    let (_tmp, vault_id, store, vault, src) = setup_watch(&base).await;
    // Seed an expired token the stub will reject.
    store
        .set_sync_token(vault_id, &src.source_id, Some("stale-token"))
        .await
        .unwrap();

    assert_eq!(
        poll_once(&store, &vault, &src, 1 << 20)
            .await
            .expect("stale-token poll must resync, not fail"),
        1
    );
    let sync = store
        .get_sync_state(vault_id, &src.source_id)
        .await
        .unwrap()
        .expect("sync row");
    assert!(
        !sync.sync_unsupported,
        "stale token must NEVER latch sync_unsupported (Pitfall 5)"
    );
    assert_eq!(sync.token.as_deref(), Some("http://stub/sync/1"));
    assert_eq!(sync.last_error, None, "resync is not an error");

    let reqs = stub.requests();
    let posts: Vec<&(String, String, Option<String>, String)> =
        reqs.iter().filter(|(m, ..)| m == "POST").collect();
    assert_eq!(
        posts.len(),
        2,
        "one failed report + exactly one retry: {reqs:?}"
    );
    assert!(
        posts[0].3.contains("stale-token"),
        "first attempt used stored token"
    );
    assert!(
        posts[1].3.contains("<d:sync-token></d:sync-token>"),
        "retry carries the empty token: {}",
        posts[1].3
    );
    assert!(
        !reqs.iter().any(|(m, ..)| m == "PROPFIND"),
        "resync stays on the sync path: {reqs:?}"
    );
}

// ---------------------------------------------------------------------------
// 04 review fix H1 — a per-file failure must not wedge the source nor drop
// log.md lines already accumulated for successful deltas.
// ---------------------------------------------------------------------------

/// H1(i)+(ii) walk path: b.md fails its GET persistently; a.md (before) and
/// c.md (after) must still enqueue, their log lines must be flushed even
/// though the poll returns Err, and the failure is recorded in last_error.
#[tokio::test]
async fn failing_file_does_not_wedge_the_walk() {
    let (base, stub) = stub_webdav();
    stub.insert("a.md", b"good a", "\"a\"");
    stub.insert("b.md", b"bad b", "\"b\"");
    stub.insert("c.md", b"good c", "\"c\"");
    stub.fail_get("b.md");
    let (_tmp, vault_id, store, vault, src) = setup_watch(&base).await;

    poll_once(&store, &vault, &src, 1 << 20)
        .await
        .expect_err("failing file must still surface as poll Err");

    // a.md and c.md enqueue despite b.md failing in the middle of the loop.
    let jobs = store.list_jobs(vault_id, Some("pending")).await.unwrap();
    assert_eq!(jobs.len(), 2, "a.md + c.md must enqueue despite b.md");

    // Log lines for the successful deltas are flushed BEFORE the Err.
    let log = std::fs::read_to_string(vault.root.join("log.md")).expect("log.md");
    assert!(log.contains("added: a.md"), "prior delta logged: {log}");
    assert!(log.contains("added: c.md"), "later delta logged: {log}");
    assert!(!log.contains("added: b.md"), "failed file not logged: {log}");

    // The failure is recorded (sanitized) for the operator.
    let sync = store
        .get_sync_state(vault_id, &src.source_id)
        .await
        .unwrap()
        .expect("sync row");
    let err = sync.last_error.expect("last_error recorded");
    assert!(err.contains("b.md"), "last_error names the failing file: {err}");

    // Next poll: b.md still fails, but a.md/c.md are NOT re-enqueued —
    // their state rows were written, so only b.md replays.
    poll_once(&store, &vault, &src, 1 << 20)
        .await
        .expect_err("b.md still failing");
    assert_eq!(
        store.list_jobs(vault_id, None).await.unwrap().len(),
        2,
        "no re-enqueue storm for the applied files"
    );
}

/// H1 sync path: a failing file inside a sync report must not drop the log
/// lines of the deltas applied before it, and the token must NOT advance
/// (the delta replays next poll, absorbed by the sha256 gate).
#[tokio::test]
async fn sync_failing_file_flushes_log_and_holds_token() {
    let (base, stub) = stub_webdav();
    stub.insert("a.md", b"good a", "\"a\"");
    stub.insert("b.md", b"bad b", "\"b\"");
    stub.fail_get("b.md");
    stub.set_sync_mode(SyncMode::Enabled {
        script: vec![SyncReply {
            items: vec![
                ("a.md".to_string(), ItemStatus::Ok),
                ("b.md".to_string(), ItemStatus::Ok),
            ],
            token: "http://stub/sync/1".to_string(),
        }],
    });
    let (_tmp, vault_id, store, vault, src) = setup_watch(&base).await;

    poll_once(&store, &vault, &src, 1 << 20)
        .await
        .expect_err("failing sync file must surface as poll Err");

    let jobs = store.list_jobs(vault_id, Some("pending")).await.unwrap();
    assert_eq!(jobs.len(), 1, "a.md enqueued despite b.md failing");
    let log = std::fs::read_to_string(vault.root.join("log.md")).expect("log.md");
    assert!(log.contains("added: a.md"), "successful delta logged: {log}");

    let sync = store
        .get_sync_state(vault_id, &src.source_id)
        .await
        .unwrap()
        .expect("sync row");
    assert!(
        sync.token.is_none(),
        "token must not advance when a delta failed: {sync:?}"
    );

    // Replay: a.md absorbed by the sha256 gate (no re-enqueue), b.md fails
    // again — poll still Err, still exactly one job.
    poll_once(&store, &vault, &src, 1 << 20)
        .await
        .expect_err("b.md still failing");
    assert_eq!(store.list_jobs(vault_id, None).await.unwrap().len(), 1);
}

/// H1 dotfile edge: a file literally named `.md` has an empty stem which the
/// pipeline sanitizer refuses. It must become a once-only skip note, not a
/// fatal per-file error that wedges every later file.
#[tokio::test]
async fn dotfile_md_is_skipped_not_fatal() {
    let (base, stub) = stub_webdav();
    stub.insert(".md", b"dot", "\"d\"");
    stub.insert("ok.md", b"ok", "\"o\"");
    let (_tmp, vault_id, store, vault, src) = setup_watch(&base).await;

    assert_eq!(
        poll_once(&store, &vault, &src, 1 << 20)
            .await
            .expect("poll must not fail on a dotfile"),
        1,
        "ok.md enqueues; .md is skipped"
    );
    let log = std::fs::read_to_string(vault.root.join("log.md")).expect("log.md");
    assert!(
        log.contains("skipped (empty file stem): .md"),
        "dotfile skip note: {log}"
    );
    assert!(log.contains("added: ok.md"), "later file applied: {log}");

    // Marker row written → second poll is a silent no-op (note logged once).
    assert_eq!(
        poll_once(&store, &vault, &src, 1 << 20)
            .await
            .expect("second poll"),
        0
    );
    let log2 = std::fs::read_to_string(vault.root.join("log.md")).expect("log.md");
    assert_eq!(
        log2.lines().filter(|l| l.contains("empty file stem")).count(),
        1,
        "skip note logged exactly once: {log2}"
    );
    assert_eq!(store.list_jobs(vault_id, None).await.unwrap().len(), 1);
}
