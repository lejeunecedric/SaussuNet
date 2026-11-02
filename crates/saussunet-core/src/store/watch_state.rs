//! `watch_state` table: per-file watcher memory for WebDAV sources (SRCC-02).
//! Etags/sizes/sha256 of the last-seen tree so a restart resumes with zero
//! re-scan. Pattern mirrors `store/sync_state.rs` (FromRow row struct,
//! upsert-safe INSERT..ON CONFLICT, parameter-bound queries only).

use sqlx::FromRow;

use crate::error::Result;

use super::Store;

/// One row of `watch_state`: a path the watcher saw on its last complete walk.
#[derive(Debug, Clone, FromRow)]
pub struct WatchRow {
    pub vault_id: i64,
    pub source_id: String,
    pub path: String,
    pub is_dir: bool,
    pub etag: Option<String>,
    pub size: Option<i64>,
    pub sha256: Option<String>,
    pub last_seen_at: String,
}

impl Store {
    /// Upsert one per-file row on key (vault_id, source_id, path). This is the
    /// primary write path (crash-safe order: called after each `insert_job`).
    #[allow(clippy::too_many_arguments)]
    pub async fn upsert_watch_row(
        &self,
        vault_id: i64,
        source_id: &str,
        path: &str,
        is_dir: bool,
        etag: Option<&str>,
        size: Option<i64>,
        sha256: Option<&str>,
        last_seen_at: &str,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO watch_state \
                (vault_id, source_id, path, is_dir, etag, size, sha256, last_seen_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(vault_id, source_id, path) DO UPDATE SET \
                is_dir = excluded.is_dir, \
                etag = excluded.etag, \
                size = excluded.size, \
                sha256 = excluded.sha256, \
                last_seen_at = excluded.last_seen_at",
        )
        .bind(vault_id)
        .bind(source_id)
        .bind(path)
        .bind(is_dir)
        .bind(etag)
        .bind(size)
        .bind(sha256)
        .bind(last_seen_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The full per-source snapshot the poller diffs against before a walk.
    pub async fn list_watch_rows(
        &self,
        vault_id: i64,
        source_id: &str,
    ) -> Result<Vec<WatchRow>> {
        let rows = sqlx::query_as::<_, WatchRow>(
            "SELECT vault_id, source_id, path, is_dir, etag, size, sha256, last_seen_at \
             FROM watch_state WHERE vault_id = ? AND source_id = ? ORDER BY path",
        )
        .bind(vault_id)
        .bind(source_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Removal detection for a single path; a missing key is a no-op.
    pub async fn delete_watch_row(&self, vault_id: i64, source_id: &str, path: &str) -> Result<()> {
        sqlx::query("DELETE FROM watch_state WHERE vault_id = ? AND source_id = ? AND path = ?")
            .bind(vault_id)
            .bind(source_id)
            .bind(path)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Diff-and-swap after a COMPLETED walk (D-11 atomicity): one transaction —
    /// DELETE all rows for (vault_id, source_id) then insert the fresh set.
    /// Never call this on a partial/failed walk (stale rows must survive).
    pub async fn replace_watch_rows(
        &self,
        vault_id: i64,
        source_id: &str,
        rows: &[WatchRow],
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM watch_state WHERE vault_id = ? AND source_id = ?")
            .bind(vault_id)
            .bind(source_id)
            .execute(&mut *tx)
            .await?;
        for r in rows {
            sqlx::query(
                "INSERT INTO watch_state \
                    (vault_id, source_id, path, is_dir, etag, size, sha256, last_seen_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(r.vault_id)
            .bind(&r.source_id)
            .bind(&r.path)
            .bind(r.is_dir)
            .bind(&r.etag)
            .bind(r.size)
            .bind(&r.sha256)
            .bind(&r.last_seen_at)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn mem_store() -> Store {
        let store = Store::connect_memory().await.expect("in-memory store");
        store.ensure_vault(1, "test").await.expect("vault row");
        store
    }

    /// Upsert → list round-trip; re-upsert on the same key overwrites
    /// etag/size/sha256 (the per-file diff gate, D-07).
    #[tokio::test]
    async fn watch_row_roundtrip() {
        let store = mem_store().await;
        assert!(store.list_watch_rows(1, "s1").await.unwrap().is_empty());

        store
            .upsert_watch_row(1, "s1", "notes/a.md", false, Some("\"e1\""), Some(10), Some("aa11"), "2026-01-01T00:00:00Z")
            .await
            .unwrap();
        let rows = store.list_watch_rows(1, "s1").await.unwrap();
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.vault_id, 1);
        assert_eq!(r.source_id, "s1");
        assert_eq!(r.path, "notes/a.md");
        assert!(!r.is_dir);
        assert_eq!(r.etag.as_deref(), Some("\"e1\""));
        assert_eq!(r.size, Some(10));
        assert_eq!(r.sha256.as_deref(), Some("aa11"));
        assert_eq!(r.last_seen_at, "2026-01-01T00:00:00Z");

        // Re-upsert same key: overwrites, does not duplicate.
        store
            .upsert_watch_row(1, "s1", "notes/a.md", false, Some("\"e2\""), Some(20), Some("bb22"), "2026-01-02T00:00:00Z")
            .await
            .unwrap();
        let rows = store.list_watch_rows(1, "s1").await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].etag.as_deref(), Some("\"e2\""));
        assert_eq!(rows[0].size, Some(20));
        assert_eq!(rows[0].sha256.as_deref(), Some("bb22"));
        assert_eq!(rows[0].last_seen_at, "2026-01-02T00:00:00Z");
    }

    /// Diff-and-swap after a COMPLETED walk (D-11): replace with a fresh set
    /// leaves exactly the new rows — old ones are gone.
    #[tokio::test]
    async fn replace_watch_rows_swaps_atomically() {
        let store = mem_store().await;
        for p in ["a.md", "b.md", "c.md"] {
            store
                .upsert_watch_row(1, "s1", p, false, None, None, None, "2026-01-01T00:00:00Z")
                .await
                .unwrap();
        }
        let fresh = vec![
            WatchRow {
                vault_id: 1,
                source_id: "s1".into(),
                path: "b.md".into(),
                is_dir: false,
                etag: Some("\"eb\"".into()),
                size: Some(2),
                sha256: None,
                last_seen_at: "2026-01-02T00:00:00Z".into(),
            },
            WatchRow {
                vault_id: 1,
                source_id: "s1".into(),
                path: "sub".into(),
                is_dir: true,
                etag: None,
                size: None,
                sha256: None,
                last_seen_at: "2026-01-02T00:00:00Z".into(),
            },
        ];
        store.replace_watch_rows(1, "s1", &fresh).await.unwrap();
        let rows = store.list_watch_rows(1, "s1").await.unwrap();
        let paths: Vec<&str> = rows.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(paths, vec!["b.md", "sub"]);
        assert!(rows.iter().find(|r| r.path == "sub").unwrap().is_dir);
        // Other sources are untouched by a swap on s1.
        store
            .upsert_watch_row(1, "s2", "keep.md", false, None, None, None, "t")
            .await
            .unwrap();
        store.replace_watch_rows(1, "s1", &[]).await.unwrap();
        assert!(store.list_watch_rows(1, "s1").await.unwrap().is_empty());
        assert_eq!(store.list_watch_rows(1, "s2").await.unwrap().len(), 1);
    }

    /// Deletion is scoped to exactly (vault_id, source_id, path).
    #[tokio::test]
    async fn delete_watch_row_removes_only_its_key() {
        let store = mem_store().await;
        store
            .upsert_watch_row(1, "s1", "a.md", false, None, None, None, "t")
            .await
            .unwrap();
        store
            .upsert_watch_row(1, "s2", "a.md", false, None, None, None, "t")
            .await
            .unwrap();
        store.delete_watch_row(1, "s1", "a.md").await.unwrap();
        assert!(store.list_watch_rows(1, "s1").await.unwrap().is_empty());
        assert_eq!(store.list_watch_rows(1, "s2").await.unwrap().len(), 1);
        // Deleting a non-existent key is a no-op, not an error.
        store.delete_watch_row(1, "s1", "missing.md").await.unwrap();
    }
}
