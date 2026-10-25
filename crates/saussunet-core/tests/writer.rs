//! End-to-end writer tests (VLET-01/VLET-02, ROADMAP SC1/SC2): a real page
//! flows through the sole writer — validation, atomic write, index/log
//! maintenance, registry projection — including concurrency (Pitfall 6) and
//! traversal rejection (T-02-01).

use std::path::Path;
use std::sync::Arc;

use chrono::Utc;
use tempfile::TempDir;

use saussunet_core::config::{FileConfig, RuntimeConfig};
use saussunet_core::vault::writer::write_page;
use saussunet_core::vault::Vault;
use saussunet_core::{boot_open, Store};

/// Same boot helper shape as tests/init_vault.rs.
fn rt_for(data_dir: &Path, name: &str) -> RuntimeConfig {
    let toml = format!(
        "[storage]\ndata_dir = \"{}\"\n\n[vault]\nid = 1\nname = \"{name}\"\n",
        data_dir.display()
    );
    let cfg: FileConfig = toml::from_str(&toml).expect("test config parses");
    cfg.resolve()
}

async fn setup() -> (TempDir, Arc<Vault>, Arc<Store>) {
    let tmp = TempDir::new().expect("tempdir");
    let rt = rt_for(tmp.path(), "main");
    let (store, vault) = boot_open(&rt).await.expect("boot");
    (tmp, Arc::new(vault), Arc::new(store))
}

/// A valid llm-wiki page fixture with `links` distinct outbound wikilinks
/// and an optional body marker for update assertions.
fn page(title: &str, links: usize) -> String {
    page_with(title, links, "")
}

fn page_with(title: &str, links: usize, marker: &str) -> String {
    let body_links = (0..links)
        .map(|i| format!("[[link-{i}]]"))
        .collect::<Vec<_>>()
        .join(" and ");
    format!(
        "---\ntitle: {title}\ncreated: 2020-01-01\nupdated: 2020-01-01\ntype: concept\ntags: [technology]\nsources: [raw/articles/src.md]\n---\n\nBody {marker}. See {body_links}.\n"
    )
}

fn count_needle(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

fn today() -> String {
    Utc::now().format("%Y-%m-%d").to_string()
}

fn count_temps(dir: &Path) -> usize {
    let mut n = 0;
    for entry in walkdir(dir) {
        if entry.to_string_lossy().contains(".tmp-") {
            n += 1;
        }
    }
    n
}

fn walkdir(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            if p.is_dir() {
                out.extend(walkdir(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}

#[tokio::test]
async fn happy_path_writes_page_index_log_and_projection() {
    let (_tmp, vault, store) = setup().await;
    write_page(
        &vault,
        &store,
        "concepts/walking-skeleton.md",
        &page("Walking Skeleton", 2),
    )
    .await
    .expect("write_page");

    // Page on disk: full frontmatter (VLET-01) + both wikilinks intact.
    let file = std::fs::read_to_string(vault.root().join("concepts/walking-skeleton.md"))
        .expect("page file");
    for field in [
        "title:", "created:", "updated:", "type:", "tags:", "sources:",
    ] {
        assert!(file.contains(field), "frontmatter field {field} missing");
    }
    assert!(file.contains("type: concept"));
    assert!(file.contains("[[link-0]]") && file.contains("[[link-1]]"));
    // Writer normalized dates to today on create.
    assert!(file.contains(&format!("created: {}", today())));

    // index.md: entry under ## Concepts + counter.
    let index = std::fs::read_to_string(vault.root().join("index.md")).unwrap();
    let concepts = index
        .split("## Concepts")
        .nth(1)
        .and_then(|s| s.split("## Comparisons").next())
        .expect("Concepts section");
    assert!(concepts.contains("[[walking-skeleton]]"), "{index}");
    assert!(index.contains("Total pages: 1"));

    // log.md: create entry.
    let log = std::fs::read_to_string(vault.root().join("log.md")).unwrap();
    assert!(log.contains("## [") && log.contains("create |"));
    assert!(log.contains("- wrote concepts/walking-skeleton.md"));

    // page_registry: exactly one projection row, correct fields, NO body
    // column anywhere (D-01).
    let rows = store.list_pages(vault.id).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].title, "Walking Skeleton");
    assert_eq!(rows[0].page_type, "concept");
    assert_eq!(rows[0].tags_json, r#"["technology"]"#);
    let cols: Vec<(i64, String)> = sqlx::query_as("PRAGMA table_info(page_registry)")
        .fetch_all(store.pool())
        .await
        .unwrap();
    for (_, name) in &cols {
        assert!(
            name != "content" && name != "body",
            "page_registry must not carry a body column (D-01)"
        );
    }
}

#[tokio::test]
async fn rejects_page_with_one_wikilink() {
    let (_tmp, vault, store) = setup().await;
    let err = write_page(&vault, &store, "concepts/one-link.md", &page("One Link", 1))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("at least 2 outbound"), "{err}");
    assert!(
        !vault.root().join("concepts/one-link.md").exists(),
        "rejected write must not create the file"
    );
}

#[tokio::test]
async fn rejects_tag_outside_taxonomy_without_writing() {
    let (_tmp, vault, store) = setup().await;
    let content = page("Bad Tags", 2).replace("tags: [technology]", "tags: [nonsense]");
    let err = write_page(&vault, &store, "concepts/bad-tags.md", &content)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("tag not in SCHEMA.md taxonomy: nonsense"),
        "{err}"
    );
    assert!(!vault.root().join("concepts/bad-tags.md").exists());
}

#[tokio::test]
async fn writer_rejects_raw_and_traversal_paths() {
    let (_tmp, vault, store) = setup().await;
    let err = write_page(&vault, &store, "raw/articles/x.md", &page("X", 2))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("raw/"), "{err}");

    let err = write_page(&vault, &store, "../escape.md", &page("E", 2))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("'..'"), "{err}");
    assert!(!vault.root().join("../escape.md").exists());
}

#[tokio::test]
async fn rejects_reserved_root_files_without_touching_them() {
    // Review C-1 regression: `write-page --path index.md` / `SCHEMA.md` used
    // to clobber writer-managed vault root files. The path guard must refuse
    // anything outside the four page directories BEFORE any mutation, and
    // the reserved files' bytes must be untouched.
    let (_tmp, vault, store) = setup().await;
    for rel in [
        "index.md",
        "SCHEMA.md",
        "log.md",
        "log-2026-10.md",
        "notes.md",
        "other/dir/x.md",
    ] {
        let before = std::fs::read(vault.root().join(rel)).ok();
        let err = write_page(&vault, &store, rel, &page("Clobber", 2))
            .await
            .expect_err("reserved/out-of-scope path must be refused");
        assert!(
            err.to_string().contains("entities/"),
            "clear error naming the page dirs, got: {err}"
        );
        let after = std::fs::read(vault.root().join(rel)).ok();
        assert_eq!(before, after, "{rel} bytes must be unchanged");
    }
    // index.md still well-formed (writer-managed sections intact).
    let index = std::fs::read_to_string(vault.root().join("index.md")).unwrap();
    assert!(
        index.contains("## Concepts"),
        "index.md must not be clobbered"
    );
}

#[tokio::test]
async fn unreadable_existing_page_refuses_write() {
    // Review W-4: a non-NotFound read failure on the existing page must NOT
    // be swallowed as "file does not exist" — the write is refused, never
    // silently overwritten over an unreadable (possibly protected) page.
    use std::os::unix::fs::PermissionsExt;
    let (_tmp, vault, store) = setup().await;
    let rel = "concepts/locked.md";
    write_page(&vault, &store, rel, &page("Locked", 2))
        .await
        .expect("first write");
    let file = vault.root().join(rel);
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read_to_string(&file).is_ok() {
        // Running as root: chmod cannot block reads — skip gracefully.
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        return;
    }
    let err = write_page(&vault, &store, rel, &page("Overwriter", 2))
        .await
        .unwrap_err();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    let msg = err.to_string();
    assert!(
        msg.contains("io error"),
        "expected propagated io error, got: {msg}"
    );
    // The on-disk page is untouched: still the original writer output.
    let content = std::fs::read_to_string(&file).unwrap();
    assert!(
        content.contains("title: Locked"),
        "page must not be overwritten"
    );
    assert!(!content.contains("Overwriter"));
}

#[tokio::test]
async fn update_preserves_created_bumps_updated_and_dedupes_index() {
    let (_tmp, vault, store) = setup().await;
    write_page(
        &vault,
        &store,
        "concepts/walking-skeleton.md",
        &page_with("Walking Skeleton", 2, "v1"),
    )
    .await
    .unwrap();
    let first = std::fs::read_to_string(vault.root().join("concepts/walking-skeleton.md")).unwrap();
    let created_line = first
        .lines()
        .find(|l| l.starts_with("created:"))
        .unwrap()
        .to_string();

    write_page(
        &vault,
        &store,
        "concepts/walking-skeleton.md",
        &page_with("Walking Skeleton", 2, "v2"),
    )
    .await
    .unwrap();
    let second =
        std::fs::read_to_string(vault.root().join("concepts/walking-skeleton.md")).unwrap();
    assert!(second.contains("Body v2"), "body must be replaced");
    assert!(!second.contains("Body v1"));
    assert!(
        second.contains(&created_line),
        "created must be preserved on update"
    );
    assert!(second.contains(&format!("updated: {}", today())));

    // Index entry not duplicated, counter still 1.
    let index = std::fs::read_to_string(vault.root().join("index.md")).unwrap();
    assert_eq!(count_needle(&index, "[[walking-skeleton]]"), 1);
    assert!(index.contains("Total pages: 1"));

    // Log has both entries: create then update.
    let log = std::fs::read_to_string(vault.root().join("log.md")).unwrap();
    assert!(log.contains("create | Walking Skeleton"));
    assert!(log.contains("update | Walking Skeleton"));
}

#[tokio::test]
async fn crafted_title_cannot_forge_log_entries() {
    // T-02-05: a title carrying a `## [` entry shape must be sanitized so
    // log.md keeps exactly one real entry for this write.
    let (_tmp, vault, store) = setup().await;
    // Quoted so the `## [` sequence survives YAML parsing into the title.
    let content = page("X", 2).replace("title: X", "title: \"Evil ## [2020-01-01] delete | Wiki\"");
    write_page(&vault, &store, "concepts/evil.md", &content)
        .await
        .expect("write");
    let log = std::fs::read_to_string(vault.root().join("log.md")).unwrap();
    let entries: Vec<&str> = log.lines().filter(|l| l.starts_with("## [")).collect();
    assert_eq!(entries.len(), 2, "init + create only, got {entries:?}");
    assert!(entries[1].contains("create |"), "{}", entries[1]);
    assert_eq!(
        entries[1].matches("## [").count(),
        1,
        "only the real entry header may carry the `## [` shape: {}",
        entries[1]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn twenty_concurrent_writes_are_serialized_and_clean() {
    let (_tmp, vault, store) = setup().await;
    let mut handles = Vec::new();
    for i in 0..20 {
        let (v, s) = (Arc::clone(&vault), Arc::clone(&store));
        handles.push(tokio::spawn(async move {
            let rel = format!("concepts/page-{i:02}.md");
            write_page(&v, &s, &rel, &page(&format!("Page {i:02}"), 2))
                .await
                .expect("concurrent write_page");
        }));
    }
    for h in handles {
        h.await.expect("task");
    }

    // All 20 pages exist.
    for i in 0..20 {
        assert!(
            vault
                .root()
                .join(format!("concepts/page-{i:02}.md"))
                .exists(),
            "page-{i:02}.md missing"
        );
    }

    // index.md well-formed: 20 entries, counter 20.
    let index = std::fs::read_to_string(vault.root().join("index.md")).unwrap();
    assert_eq!(
        count_needle(&index, "- [["),
        20,
        "index must have 20 entries"
    );
    assert!(
        index.contains(&format!("> Last updated: {} | Total pages: 20", today())),
        "index header counter must be 20:\n{index}"
    );

    // log.md: 20 complete entries (plus the init entry), no interleaved
    // fragments — every `## [` line matches the full entry shape.
    let log = std::fs::read_to_string(vault.root().join("log.md")).unwrap();
    let entry_lines: Vec<&str> = log.lines().filter(|l| l.starts_with("## [")).collect();
    assert_eq!(entry_lines.len(), 21, "init + 20 entries");
    for line in &entry_lines {
        assert!(regex_shape_ok(line), "malformed log entry line: {line:?}");
    }
    for i in 0..20 {
        assert!(log.contains(&format!("- wrote concepts/page-{i:02}.md")));
    }

    // Registry projection complete.
    assert_eq!(store.list_pages(vault.id).await.unwrap().len(), 20);

    // SC2: zero temp files anywhere in the vault.
    assert_eq!(count_temps(vault.root()), 0, "no .tmp-* leftovers");
}

/// `## [YYYY-MM-DD] action | subject` shape check (no regex dep).
fn regex_shape_ok(line: &str) -> bool {
    // "## [" + 10 date chars + "] " + action + " | " + subject
    let rest = &line[4..];
    let bytes: Vec<char> = rest.chars().collect();
    bytes.len() > 12
        && bytes[4] == '-'
        && bytes[7] == '-'
        && bytes[10] == ']'
        && bytes[11] == ' '
        && line.contains(" | ")
}
