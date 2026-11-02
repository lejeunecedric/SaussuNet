//! Pure walk-vs-state differ (no DB, no HTTP — grep-proof: no reqwest, no
//! sqlx, no fs). `diff_walk` answers "what changed" given a COMPLETED walk
//! and the persisted `watch_state` rows; the poller (04-04) maps
//! `DavEntry`/`WatchRow` into these plain structs.

#[cfg(test)]
mod tests {
    use super::{diff_walk, StateEntry, WalkEntry, WatchDelta};

    fn walk(path: &str, etag: Option<&str>, size: Option<i64>, is_dir: bool) -> WalkEntry {
        WalkEntry {
            path: path.into(),
            etag: etag.map(Into::into),
            size,
            is_dir,
        }
    }

    fn state(path: &str, etag: Option<&str>, size: Option<i64>, is_dir: bool) -> StateEntry {
        StateEntry {
            path: path.into(),
            etag: etag.map(Into::into),
            size,
            is_dir,
            sha256: None,
        }
    }

    const ALLOW: &[&str] = &["md", "txt", "pdf"];
    fn allow() -> Vec<String> {
        ALLOW.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn added_when_path_absent_from_state() {
        let deltas = diff_walk(&[walk("note.md", Some("\"a\""), Some(1), false)], &[], &allow());
        assert_eq!(deltas.len(), 1);
        match &deltas[0] {
            WatchDelta::Added(w) => assert_eq!(w.path, "note.md"),
            other => panic!("expected Added, got {other:?}"),
        }
    }

    #[test]
    fn changed_on_etag_mismatch_even_weak() {
        // Weak rotation W/"a" -> "a" is STILL Changed: the sha256 gate
        // downstream decides whether it is worth a job (D-07) — never trust
        // etag equality alone in the other direction either.
        let deltas = diff_walk(
            &[walk("note.md", Some("\"a\""), Some(1), false)],
            &[state("note.md", Some("W/\"a\""), Some(1), false)],
            &allow(),
        );
        assert_eq!(deltas.len(), 1);
        assert!(matches!(&deltas[0], WatchDelta::Changed(w) if w.path == "note.md"));
    }

    #[test]
    fn unchanged_when_etag_and_size_equal() {
        // T9 substrate: a steady-state poll produces ZERO deltas.
        let deltas = diff_walk(
            &[walk("note.md", Some("\"a\""), Some(1), false)],
            &[state("note.md", Some("\"a\""), Some(1), false)],
            &allow(),
        );
        assert!(deltas.is_empty(), "expected no delta, got {deltas:?}");
    }

    #[test]
    fn removed_when_in_state_absent_from_walk() {
        let deltas = diff_walk(
            &[],
            &[state("gone.md", Some("\"g\""), Some(2), false)],
            &allow(),
        );
        assert_eq!(deltas.len(), 1);
        match &deltas[0] {
            WatchDelta::Removed(s) => assert_eq!(s.path, "gone.md"),
            other => panic!("expected Removed, got {other:?}"),
        }
    }

    #[test]
    fn skipped_for_extension_outside_allowlist() {
        // D-08: new notes.docx is Skipped, not Added (poller logs once, 04-05).
        let deltas = diff_walk(&[walk("notes.docx", Some("\"d\""), Some(9), false)], &[], &allow());
        assert_eq!(deltas.len(), 1);
        match &deltas[0] {
            WatchDelta::Skipped(w) => assert_eq!(w.path, "notes.docx"),
            other => panic!("expected Skipped, got {other:?}"),
        }
    }

    #[test]
    fn dirs_never_enqueued() {
        // A walk collection matching nothing in state produces no Added —
        // dirs are state bookkeeping only.
        let deltas = diff_walk(&[walk("papers", None, None, true)], &[], &allow());
        assert!(deltas.is_empty(), "dirs must never delta: {deltas:?}");
    }

    #[test]
    fn extension_compare_is_case_insensitive_and_dotless() {
        // Allowlist is lowercase dotless (D-08 normalization); a walk file
        // with an uppercase extension must still match.
        let deltas = diff_walk(
            &[walk("report.PDF", Some("\"p\""), Some(3), false)],
            &[],
            &allow(),
        );
        assert_eq!(deltas.len(), 1);
        assert!(
            matches!(&deltas[0], WatchDelta::Added(w) if w.path == "report.PDF"),
            "expected Added, got {deltas:?}"
        );
    }

    #[test]
    fn removed_dir_children_each_removed() {
        // Walk came back empty (files deleted server-side): every state child
        // is individually Removed — the caller guarantees a COMPLETED walk.
        let st = vec![
            state("papers", None, None, true),
            state("papers/a.md", Some("\"1\""), Some(1), false),
            state("papers/b.md", Some("\"2\""), Some(2), false),
        ];
        let deltas = diff_walk(&[], &st, &allow());
        let removed: Vec<&str> = deltas
            .iter()
            .filter_map(|d| match d {
                WatchDelta::Removed(s) => Some(s.path.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(removed, vec!["papers", "papers/a.md", "papers/b.md"]);
    }
}
