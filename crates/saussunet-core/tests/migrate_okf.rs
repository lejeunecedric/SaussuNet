//! Plan 04.1-05 T11/T12/T13 — `migrate-okf`: in-place llm-wiki → OKF
//! conversion with dry-run default, terminal refusals, and B3Wiki parity.
//!
//! T11 (`migrate_mini_bundle`) ports `B3Wiki/tests/test_to_okf.py::
//! TestConvertEndToEnd::test_mini_bundle` with the D-12/D-17 expectation
//! changes: footnotes instead of the `# Citations` heading, and the quoted
//! `okf_version: "0.2"` index frontmatter.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use saussunet_core::config::{FileConfig, RuntimeConfig};
use saussunet_core::vault::{frontmatter, ghash, okf, Vault};
use saussunet_core::{boot_open, migrate, Store};

/// Boot a store+vault rooted at `tmp/data/vaults/1` (same harness shape as
/// `tests/pipeline_thresholds.rs::setup_with_config`).
async fn setup(tmp: &TempDir) -> (Store, Vault) {
    let toml = format!(
        "[storage]\ndata_dir = \"{}\"\n\n[vault]\nid = 1\nname = \"main\"\n",
        tmp.path().display()
    );
    let cfg: FileConfig = toml::from_str(&toml).expect("config parses");
    let rt: RuntimeConfig = cfg.resolve().expect("resolve");
    let (store, vault) = boot_open(&rt).await.expect("boot");
    (store, vault)
}

/// Write `rel` under `root`, creating parent dirs.
fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, text).unwrap();
}

/// Snapshot every file's (bytes, mtime) under `root` for zero-write proofs.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, (String, std::time::SystemTime)> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                // .git churns on its own (git status refreshes the index
                // stat cache) — the zero-write claim is about vault files.
                if path.file_name().is_some_and(|n| n == ".git") {
                    continue;
                }
                stack.push(path);
                continue;
            }
            let meta = std::fs::metadata(&path).unwrap();
            out.insert(
                path.clone(),
                (
                    std::fs::read_to_string(&path).unwrap_or_default(),
                    meta.modified().unwrap(),
                ),
            );
        }
    }
    out
}

fn assert_unchanged(before: &BTreeMap<PathBuf, (String, std::time::SystemTime)>, root: &Path) {
    let after = snapshot(root);
    assert_eq!(before, &after, "dry-run/refusal must modify zero files");
}

/// Build the synthetic llm-wiki vault (port of TestConvertEndToEnd plus the
/// T11 extras: 2 concepts + 1 entity, wikilinks + `^[raw/...]` markers,
/// lowercase types, created/updated dates, old-format index + log, old
/// SCHEMA.md, one protected page, one dirty-frontmatter page).
fn build_fixture(root: &Path) {
    write(
        root,
        "log.md",
        "# Log\n\n> Format: `## [YYYY-MM-DD] action | subject`\n\n\
         ## [2026-10-20] create | Wiki initialized\n- Domain: x\n",
    );
    write(
        root,
        "index.md",
        "# Index\n\n- [[players/a|A]] — A page.\n- [[alpha]] — Alpha concept.\n",
    );
    write(
        root,
        "SCHEMA.md",
        "# Schema\n\n## Tag Taxonomy\n- technology: ai, music\n",
    );
    write(
        root,
        "entities/players/a.md",
        "---\ntitle: A\ntype: entity\ntags: [person]\nsources: [raw/articles/r.md]\n\
         created: 2026-10-19\nupdated: 2026-10-19\n---\n\nSee [[r]] and [[alpha]]. \
         Claim one.^[raw/articles/r.md]\n",
    );
    write(
        root,
        "concepts/alpha.md",
        "---\ntitle: Alpha\ntype: concept\ntags: [tech]\ncreated: 2026-10-18\n\
         updated: 2026-10-19\n---\n\nAlpha is a test concept.^[raw/articles/r.md] \
         See [[beta]] and [[a]].\n",
    );
    // Dirty YAML title (Pitfall 6): strict parse fails, parse_lenient must rescue.
    write(
        root,
        "concepts/beta.md",
        "---\ntitle: \"Brother\" Jack McDuff\ntype: concept\ntags: [jazz]\n\
         created: 2026-10-17\nupdated: 2026-10-18\n---\n\nBrother was a jazz organ \
         trio album. See [[alpha]] and [[a]].\n",
    );
    // Protected page: human-owned AND already vocabulary-conformant — the
    // converter must leave it byte-identical (its [[alpha]] link untouched).
    write(
        root,
        "concepts/locked.md",
        "---\ntitle: Locked\ntype: Concept\nprotected: true\ncreated: 2026-10-17\n\
         updated: 2026-10-17\n---\n\nHuman text with a [[alpha]] link.\n",
    );
    write(
        root,
        "raw/articles/r.md",
        "---\nsource_url: https://x.org\ningested: 2026-10-19\nsha256: abc\n---\n\n\
         # R source\n",
    );
}

#[tokio::test]
async fn migrate_mini_bundle() {
    let tmp = TempDir::new().expect("tempdir");
    let backup = TempDir::new().expect("backup dir");
    let (store, vault) = setup(&tmp).await;
    let root = vault.root().to_path_buf();
    build_fixture(&root);

    // Threat model: a symlink escaping the root is skipped and reported, never
    // followed.
    let outside = TempDir::new().expect("outside");
    write(
        outside.path(),
        "secret.md",
        "---\ntype: Concept\ntitle: Outside\n---\n\nbody\n",
    );
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        outside.path().join("secret.md"),
        root.join("concepts/ghost.md"),
    )
    .expect("symlink");

    // ---- (a) dry-run: report only, ZERO writes -----------------------------
    let before = snapshot(&root);
    let report = migrate::dry_run(&vault).await.expect("dry_run");
    assert!(
        !report.files_to_change.is_empty(),
        "dry-run must list files to change: {report:?}"
    );
    assert!(
        !report.rule_counts.is_empty(),
        "per-rule counts must be populated"
    );
    assert!(
        matches!(report.verdict, migrate::Verdict::WouldConvert),
        "verdict: {:?}",
        report.verdict
    );
    assert_unchanged(&before, &root);
    let rels: Vec<&str> = report
        .files_to_change
        .iter()
        .map(|f| f.rel.as_str())
        .collect();
    for expected in [
        "entities/players/a.md",
        "concepts/alpha.md",
        "concepts/beta.md",
        "raw/articles/r.md",
        "index.md",
        "log.md",
        "SCHEMA.md",
    ] {
        assert!(rels.contains(&expected), "missing {expected} in {rels:?}");
    }
    // (e) protected page skipped + reported; (i) symlink skipped + reported.
    assert!(
        report
            .skipped_protected
            .contains(&"concepts/locked.md".to_string()),
        "protected page must be reported: {:?}",
        report.skipped_protected
    );
    #[cfg(unix)]
    assert!(
        report
            .skipped_symlinks
            .contains(&"concepts/ghost.md".to_string()),
        "escaping symlink must be reported: {:?}",
        report.skipped_symlinks
    );
    // (h) report JSON hygiene: relative paths + rule ids only — no absolute
    // paths, no body text.
    let json = serde_json::to_string(&report).expect("report serializes");
    assert!(
        !json.contains(&root.display().to_string())
            && !json.contains(&tmp.path().display().to_string()),
        "report JSON leaked an absolute path: {json}"
    );
    assert!(
        !json.contains("R source") && !json.contains("Human text"),
        "report JSON leaked page body text: {json}"
    );

    // ---- (b) apply: convert in place ---------------------------------------
    let report = migrate::apply(&vault, &store, Some(backup.path()))
        .await
        .expect("apply");
    assert!(
        matches!(
            report.verdict,
            migrate::Verdict::Clean | migrate::Verdict::WouldConvert
        ),
        "apply verdict: {:?}",
        report.verdict
    );

    let page_a = std::fs::read_to_string(root.join("entities/players/a.md")).unwrap();
    let (a_yaml, a_body) = frontmatter::split(&page_a);
    assert!(
        a_yaml.contains("type: Entity"),
        "capitalized type: {a_yaml}"
    );
    assert!(
        a_yaml.contains("description:"),
        "D-06 description: {a_yaml}"
    );
    assert!(
        a_yaml.contains("timestamp: '2026-10-19T00:00:00Z'")
            || a_yaml.contains("timestamp: \"2026-10-19T00:00:00Z\"")
            || a_yaml.contains("timestamp: 2026-10-19T00:00:00Z"),
        "timestamp ← updated (date → T00:00:00Z): {a_yaml}"
    );
    assert!(a_yaml.contains("generated:"), "generated block: {a_yaml}");
    assert!(a_yaml.contains("process:saussunet/"), "actor: {a_yaml}");
    assert!(!page_a.contains("[["), "zero wikilinks on disk: {page_a}");
    assert!(
        page_a.contains("[R source](/raw/articles/r.md)")
            || page_a.contains("[r](/raw/articles/r.md)")
            || page_a.contains("[r source](/raw/articles/r.md)"),
        "wikilink to raw resolved: {page_a}"
    );
    assert!(
        a_body.contains("[^r]"),
        "footnote marker keyed to sources[].id: {a_body}"
    );
    assert!(a_body.contains("[^r]:"), "footnote definition: {a_body}");
    assert!(
        !page_a.contains("# Citations"),
        "no v0.1 citations heading: {page_a}"
    );
    assert!(
        a_yaml.contains("resource: /raw/articles/r.md"),
        "sources[] resource: {a_yaml}"
    );
    assert!(a_yaml.contains("id: r"), "sources[] id: {a_yaml}");

    // raw page: type: Source, extension keys kept, body bytes untouched.
    let raw_page = std::fs::read_to_string(root.join("raw/articles/r.md")).unwrap();
    let (raw_yaml, raw_body) = frontmatter::split(&raw_page);
    assert!(
        raw_yaml.contains("type: Source"),
        "raw is Source: {raw_yaml}"
    );
    assert!(
        raw_yaml.contains("source_url:") && raw_yaml.contains("https://x.org"),
        "{raw_yaml}"
    );
    assert!(raw_yaml.contains("sha256: abc"), "{raw_yaml}");
    assert_eq!(raw_body, "\n# R source\n", "raw body byte-identical");

    // dirty-YAML page rescued by parse_lenient (Pitfall 6).
    let beta = std::fs::read_to_string(root.join("concepts/beta.md")).unwrap();
    assert!(beta.contains("Brother"), "lenient title preserved: {beta}");
    assert!(beta.contains("type: Concept"), "{beta}");

    // index: okf_version frontmatter + new entry format + backfilled descriptions.
    let index = std::fs::read_to_string(root.join("index.md")).unwrap();
    assert!(
        index.starts_with("---\nokf_version: \"0.2\"\n---"),
        "index frontmatter: {index}"
    );
    assert!(
        index.contains("- [A](/entities/players/a.md) — A page."),
        "index entry + backfilled description: {index}"
    );
    assert!(
        index.contains("- [Alpha](/concepts/alpha.md) — Alpha concept."),
        "index entry + backfilled description: {index}"
    );

    // log: date-grouped newest-first, old `## [` shape gone.
    let log = std::fs::read_to_string(root.join("log.md")).unwrap();
    assert!(log.contains("## 2026-10-20"), "date group kept: {log}");
    assert!(
        log.contains("* **Create**: Wiki initialized"),
        "bullet shape: {log}"
    );
    assert!(!log.contains("## ["), "old heading shape gone: {log}");

    // SCHEMA.md typed Schema (Pitfall 5).
    let schema = std::fs::read_to_string(root.join("SCHEMA.md")).unwrap();
    assert!(
        schema.starts_with("---\ntype: Schema"),
        "SCHEMA typed: {schema}"
    );

    // ---- (c) full-bundle conformance: 0 errors ------------------------------
    let errors = okf::check_bundle(&root);
    assert!(errors.is_empty(), "check_bundle errors: {errors:?}");

    // ---- (d) gen_hash re-seed over the FINAL bytes (Pitfall 2) --------------
    for rel in [
        "entities/players/a.md",
        "concepts/alpha.md",
        "concepts/beta.md",
    ] {
        let text = std::fs::read_to_string(root.join(rel)).unwrap();
        let (yaml, body) = frontmatter::split(&text);
        let fm = frontmatter::parse(yaml).unwrap();
        let stored = fm
            .gen_hash
            .unwrap_or_else(|| panic!("{rel}: gen_hash missing"));
        assert_eq!(
            stored,
            ghash::compute_map(yaml, body),
            "{rel}: stored gen_hash must equal compute_map over final bytes"
        );
    }

    // ---- (e) protected page byte-identical ----------------------------------
    let locked = std::fs::read_to_string(root.join("concepts/locked.md")).unwrap();
    assert!(
        locked.contains("Human text with a [[alpha]] link."),
        "protected page untouched: {locked}"
    );
    assert!(
        !locked.contains("generated:"),
        "protected page untouched: {locked}"
    );

    // ---- (f) reindex: SQLite matches the tree --------------------------------
    let tree_pages = [
        "entities/players/a.md",
        "concepts/alpha.md",
        "concepts/beta.md",
        "concepts/locked.md",
    ];
    let rows = store.list_pages(vault.id).await.expect("list_pages");
    let row_paths: Vec<&str> = rows.iter().map(|r| r.path.as_str()).collect();
    for rel in tree_pages {
        assert!(
            row_paths.contains(&rel),
            "reindex missing {rel}: {row_paths:?}"
        );
    }
    assert_eq!(
        rows.len(),
        tree_pages.len(),
        "reindex row count == tree pages"
    );

    // ---- (g) migration log entry under today's group -------------------------
    let log = std::fs::read_to_string(root.join("log.md")).unwrap();
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    assert!(log.contains(&format!("## {today}")), "today's group: {log}");
    assert!(log.contains("* **migrate-okf**:"), "migration entry: {log}");
    // Newest-first: today's group sits above the 2026-10-20 group.
    let today_pos = log.find(&format!("## {today}")).unwrap();
    let old_pos = log.find("## 2026-10-20").unwrap();
    assert!(today_pos < old_pos, "newest-first: {log}");
}

// ---------------------------------------------------------------------------
// Task 2 — T12 refusals, --verify, undo-path decision matrix, T13 parity.
// ---------------------------------------------------------------------------

/// Minimal already-converted vault: root index carries `okf_version`.
fn write_converted_index(root: &Path) {
    write(
        root,
        "index.md",
        "---\nokf_version: \"0.2\"\n---\n# Index\n\n- [X](/concepts/x.md) — X.\n",
    );
    write(
        root,
        "concepts/x.md",
        "---\ntype: Concept\ntitle: X\ndescription: X page.\ntimestamp: '2026-10-20T00:00:00Z'\n\
         generated:\n  by: process:b3wiki-agent\n  at: '2026-10-20T00:00:00Z'\n---\n\nX body.\n",
    );
}

/// `n` pages, `gen` of them carrying a `generated:` block (threshold fixture).
fn write_generated_pages(root: &Path, n: usize, gen: usize) {
    for i in 1..=n {
        let mut fm = format!("---\ntype: Concept\ntitle: P{i}\ncreated: 2026-10-01\n---\n\n");
        if i <= gen {
            fm.push_str("Body with generated marker.\n");
            // append generated block into the frontmatter instead
            fm = format!(
                "---\ntype: Concept\ntitle: P{i}\ngenerated:\n  by: process:b3wiki-agent\n  \
                 at: '2026-10-20T00:00:00Z'\n---\n\nBody {i}.\n"
            );
        } else {
            fm.push_str(&format!("Body {i}.\n"));
        }
        write(root, &format!("concepts/p{i}.md"), &fm);
    }
}

#[tokio::test]
async fn migrate_refusals() {
    // (a) root index carries okf_version → terminal refusal, zero writes.
    let tmp = TempDir::new().expect("tempdir");
    let (store, vault) = setup(&tmp).await;
    let root = vault.root().to_path_buf();
    write_converted_index(&root);
    let before = snapshot(&root);
    let report = migrate::dry_run(&vault).await.expect("dry_run");
    let reason = report
        .verdict
        .reason()
        .unwrap_or_else(|| panic!("expected refusal, got {:?}", report.verdict));
    assert!(reason.contains("already_converted"), "{reason}");
    assert_unchanged(&before, &root);
    let report = migrate::apply(&vault, &store, None).await.expect("apply");
    assert!(report.verdict.reason().is_some(), "apply must refuse too");
    assert_unchanged(&before, &root);

    // (b) ≥80% generated: threshold — 4/5 refuses, 3/5 proceeds.
    let tmp4 = TempDir::new().expect("tempdir4");
    let (_s4, v4) = setup(&tmp4).await;
    // The seeded SCHEMA.md would join the non-reserved denominator; the
    // boundary test counts exactly its 5 pages.
    std::fs::remove_file(v4.root().join("SCHEMA.md")).expect("drop seeded SCHEMA");
    write(
        v4.root(),
        "index.md",
        "# Index\n\n- [[p1]] — one.\n- [[p2]] — two.\n",
    );
    write_generated_pages(v4.root(), 5, 4);
    let before4 = snapshot(v4.root());
    let report = migrate::dry_run(&v4).await.expect("dry_run 4/5");
    let reason = report
        .verdict
        .reason()
        .unwrap_or_else(|| panic!("4/5 generated must refuse, got {:?}", report.verdict));
    assert!(reason.contains("already_converted"), "{reason}");
    assert_unchanged(&before4, v4.root());

    let tmp3 = TempDir::new().expect("tempdir3");
    let (_s3, v3) = setup(&tmp3).await;
    std::fs::remove_file(v3.root().join("SCHEMA.md")).expect("drop seeded SCHEMA");
    write(
        v3.root(),
        "index.md",
        "# Index\n\n- [[p1]] — one.\n- [[p2]] — two.\n",
    );
    write_generated_pages(v3.root(), 5, 3);
    let report = migrate::dry_run(&v3).await.expect("dry_run 3/5");
    assert!(
        report.verdict.reason().is_none(),
        "3/5 generated must proceed, got {:?}",
        report.verdict
    );

    // (c) git repo with a dirty worktree → refuse, even with --backup.
    let tmpg = TempDir::new().expect("tempdirg");
    let (sg, vg) = setup(&tmpg).await;
    let rg = vg.root().to_path_buf();
    build_fixture(&rg);
    let git = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(&rg)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .expect("git");
        assert!(
            status.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&status.stderr)
        );
    };
    git(&["init", "-q"]);
    git(&["-c", "user.email=t@t", "-c", "user.name=t", "add", "-A"]);
    git(&[
        "-c",
        "user.email=t@t",
        "-c",
        "user.name=t",
        "commit",
        "-q",
        "-m",
        "init",
    ]);
    std::fs::write(rg.join("concepts/alpha.md"), "# dirty\n").unwrap();
    let before_g = snapshot(&rg);
    let backup = TempDir::new().expect("backup");
    let report = migrate::apply(&vg, &sg, None).await.expect("apply dirty");
    let reason = report
        .verdict
        .reason()
        .unwrap_or_else(|| panic!("dirty worktree must refuse, got {:?}", report.verdict));
    assert!(reason.contains("dirty_worktree"), "{reason}");
    assert_unchanged(&before_g, &rg);
    let report = migrate::apply(&vg, &sg, Some(backup.path()))
        .await
        .expect("apply dirty+backup");
    assert!(
        report
            .verdict
            .reason()
            .is_some_and(|r| r.contains("dirty_worktree")),
        "dirty git refuses even with backup (D-25): {:?}",
        report.verdict
    );
    assert_unchanged(&before_g, &rg);

    // (d) non-git vault: refuse without --backup; with --backup the archive
    // exists (and contains the tree) and the conversion proceeds.
    let tmpn = TempDir::new().expect("tempdirn");
    let (sn, vn) = setup(&tmpn).await;
    let rn = vn.root().to_path_buf();
    build_fixture(&rn);
    let before_n = snapshot(&rn);
    let report = migrate::apply(&vn, &sn, None).await.expect("apply non-git");
    let reason = report.verdict.reason().unwrap_or_else(|| {
        panic!(
            "non-git without backup must refuse, got {:?}",
            report.verdict
        )
    });
    assert!(reason.contains("no_backup"), "{reason}");
    assert_unchanged(&before_n, &rn);

    let backup = TempDir::new().expect("backup");
    let report = migrate::apply(&vn, &sn, Some(backup.path()))
        .await
        .expect("apply with backup");
    assert!(report.verdict.reason().is_none(), "{:?}", report.verdict);
    let archives: Vec<_> = std::fs::read_dir(backup.path())
        .expect("backup dir")
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .ends_with(".tar.gz")
        })
        .collect();
    assert_eq!(archives.len(), 1, "exactly one archive: {archives:?}");
    let listing = std::process::Command::new("tar")
        .args(["-tzf", archives[0].to_string_lossy().as_ref()])
        .output()
        .expect("tar");
    let listing = String::from_utf8_lossy(&listing.stdout).to_string();
    assert!(
        listing.contains("concepts/alpha.md"),
        "archive must contain the tree: {listing}"
    );

    // (e) --verify on the converted vault: Clean, 0 errors, zero writes;
    // on a corrupted converted vault: errors listed, still zero writes.
    let before_v = snapshot(&rn);
    let report = migrate::verify(&vn).await.expect("verify clean");
    assert_eq!(
        report.verdict,
        migrate::Verdict::Clean,
        "{:?}",
        report.verdict
    );
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_unchanged(&before_v, &rn);

    std::fs::write(
        rn.join("concepts/alpha.md"),
        "---\ntype: [unclosed\n---\n\nbody\n",
    )
    .unwrap();
    let before_b = snapshot(&rn);
    let report = migrate::verify(&vn).await.expect("verify broken");
    assert!(!report.errors.is_empty(), "broken page must list errors");
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.contains("concepts/alpha.md")),
        "{:?}",
        report.errors
    );
    assert_ne!(report.verdict, migrate::Verdict::Clean);
    assert_unchanged(&before_b, &rn);
}

/// (f) Pure undo-path decision matrix (git-absence is unit-testable here —
/// the subprocess wrapper feeds these same inputs).
#[test]
fn undo_decision_matrix() {
    // git binary absent → MissingBinary-style error, whatever else is true.
    let err = migrate::decide_undo(false, false, false, false).expect_err("git-absent must error");
    assert!(
        matches!(&err, saussunet_core::error::CoreError::MissingBinary { binary, .. } if binary == "git"),
        "{err}"
    );
    // git repo: dirty refuses (backup does not override), clean proceeds.
    assert_eq!(
        migrate::decide_undo(true, true, true, false)
            .unwrap()
            .as_deref(),
        Some("dirty_worktree")
    );
    assert_eq!(
        migrate::decide_undo(true, true, true, true)
            .unwrap()
            .as_deref(),
        Some("dirty_worktree")
    );
    assert_eq!(
        migrate::decide_undo(true, true, false, false).unwrap(),
        None
    );
    // non-git: backup required.
    assert_eq!(
        migrate::decide_undo(true, false, false, false)
            .unwrap()
            .as_deref(),
        Some("no_backup")
    );
    assert_eq!(
        migrate::decide_undo(true, false, false, true).unwrap(),
        None
    );
}

/// Recursively copy `src` into `dst`, skipping `.git` directories.
fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap().flatten() {
        let name = entry.file_name();
        if name == ".git" {
            continue;
        }
        let from = entry.path();
        let to = dst.join(&name);
        if from.is_dir() {
            copy_tree(&from, &to);
        } else {
            std::fs::copy(&from, &to).unwrap();
        }
    }
}

/// T13 (D-29): structural parity on a real B3Wiki checkout — convert a COPY
/// and require 0 errors from the Rust checker plus the MoriNet
/// `scan_conformance` version guess `Some("0.2")`. Citation notation is
/// intentionally divergent (D-12), so this is structural parity only.
#[tokio::test]
#[ignore = "requires local /home/c/Projects/Dev/B3Wiki checkout"]
async fn migrate_b3wiki_parity() {
    let src = Path::new("/home/c/Projects/Dev/B3Wiki");
    assert!(src.exists(), "B3Wiki checkout missing at {src:?}");
    let tmp = TempDir::new().expect("tempdir");
    let (store, vault) = setup(&tmp).await;
    copy_tree(src, vault.root());

    // The B3Wiki tree is already OKF-converted upstream — the refusal gate
    // would (correctly) fire, so parity runs the conversion core directly.
    let report = migrate::convert_tree(&vault, &store)
        .await
        .expect("convert_tree");
    assert!(report.verdict.reason().is_none(), "{:?}", report.verdict);

    let errors = okf::check_bundle(vault.root());
    assert!(
        errors.is_empty(),
        "check_bundle on converted B3Wiki: {errors:?}"
    );

    // MoriNet scan_conformance rule replicated in-test (okf.rs:16,54-74):
    // root index.md/log.md reserved; version guessed when typed pages are
    // ≥80% of non-reserved and some page carries a v0.2 trust key.
    const V02_KEYS: [&str; 5] = ["generated", "verified", "sources", "status", "stale_after"];
    let (files, _skipped) = migrate::walk(vault.root()).expect("walk");
    let mut total = 0usize;
    let mut concepts = 0usize;
    let mut v02 = false;
    for (rel, path) in &files {
        if rel == "index.md" || rel == "log.md" {
            continue;
        }
        total += 1;
        let Ok(md) = std::fs::read_to_string(path) else {
            continue;
        };
        let (yaml, _) = frontmatter::split(&md);
        let Ok(fm) = serde_yml::from_str::<serde_yml::Value>(yaml) else {
            continue;
        };
        let Some(m) = fm.as_mapping() else { continue };
        let typed = m
            .get("type")
            .and_then(|t| t.as_str())
            .is_some_and(|t| !t.trim().is_empty());
        if typed {
            concepts += 1;
            if V02_KEYS.iter().any(|k| m.get(k).is_some()) {
                v02 = true;
            }
        }
    }
    assert!(total > 0, "B3Wiki copy has no pages");
    let version: Option<&str> = if total > 0 && concepts * 100 >= total * 80 {
        Some(if v02 { "0.2" } else { "0.1" })
    } else {
        None
    };
    assert_eq!(
        version,
        Some("0.2"),
        "MoriNet rule: concepts={concepts} total={total} v02={v02}"
    );
}

// ---------------------------------------------------------------------------
// H2 regression (04.1 review): `migrate-okf --apply` must leave SCHEMA.md
// with a `## Entity Subtypes` section — `write_page` hard-requires
// `parse_entity_subtypes`, so a migrated vault without one can never be
// written to again.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn migrated_bundle_accepts_writes() {
    let tmp = TempDir::new().expect("tempdir");
    let backup = TempDir::new().expect("backup dir");
    let (store, vault) = setup(&tmp).await;
    let root = vault.root().to_path_buf();
    build_fixture(&root);
    migrate::apply(&vault, &store, Some(backup.path()))
        .await
        .expect("apply");

    let schema = std::fs::read_to_string(root.join("SCHEMA.md")).unwrap();
    assert!(
        schema.contains("## Entity Subtypes"),
        "migrated SCHEMA.md must declare entity subtypes:\n{schema}"
    );

    // The generated funnel must work on the migrated vault: fresh raw
    // evidence (pipeline-shaped ISO timestamps) + wikilinks that resolve to
    // migrated pages.
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let raw_fm = saussunet_core::vault::raw::RawFrontmatter {
        title: "Fresh src".into(),
        source_url: "https://x.test/fresh".into(),
        ingested: format!("{today}T12:00:00+00:00"),
        sha256: "aa".into(),
    };
    saussunet_core::vault::raw::write_raw(&vault, None, "fresh-src", "md", &raw_fm, b"fresh body")
        .await
        .expect("write raw");
    let src = format!("raw/{today}-fresh-src.md");
    let content = format!(
        "---\ntitle: Fresh\ntype: concept\ncreated: {today}\nupdated: {today}\n\
         tags: [ai]\nsources: [{src}]\n---\n\n\
         Fresh synthesizes the source. ^[{src}] See [[alpha]] and [[beta]].\n"
    );
    saussunet_core::vault::writer::write_generated_page(&vault, &store, "concepts/fresh.md", &content)
        .await
        .expect("post-migration write_generated_page must succeed");
    assert!(root.join("concepts/fresh.md").exists(), "page must be on disk");
}
