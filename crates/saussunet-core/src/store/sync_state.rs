//! `sync_state` table: watcher state (schema + minimal APIs only, D-02;
//! behavior lands in Phase 4).

use sqlx::FromRow;

use crate::error::Result;

use super::Store;

#[derive(Debug, Clone, FromRow)]
pub struct SyncState {
    pub vault_id: i64,
    pub source_id: String,
    pub token: Option<String>,
    pub etag: Option<String>,
    pub last_poll_at: Option<String>,
    pub last_error: Option<String>,
    /// D-05: RFC 6578 try-and-fallback latch (the latch logic lands in 04-07).
    pub sync_unsupported: bool,
}

impl Store {
    #[allow(clippy::too_many_arguments)]
    pub async fn upsert_sync_state(
        &self,
        vault_id: i64,
        source_id: &str,
        token: Option<&str>,
        etag: Option<&str>,
        last_poll_at: Option<&str>,
        last_error: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO sync_state \
                (vault_id, source_id, token, etag, last_poll_at, last_error) \
             VALUES (?, ?, ?, ?, ?, ?) \
             ON CONFLICT(vault_id, source_id) DO UPDATE SET \
                token = excluded.token, \
                etag = excluded.etag, \
                last_poll_at = excluded.last_poll_at, \
                last_error = excluded.last_error",
        )
        .bind(vault_id)
        .bind(source_id)
        .bind(token)
        .bind(etag)
        .bind(last_poll_at)
        .bind(last_error)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_sync_state(
        &self,
        vault_id: i64,
        source_id: &str,
    ) -> Result<Option<SyncState>> {
        let row = sqlx::query_as::<_, SyncState>(
            "SELECT vault_id, source_id, token, etag, last_poll_at, last_error, sync_unsupported \
             FROM sync_state WHERE vault_id = ? AND source_id = ?",
        )
        .bind(vault_id)
        .bind(source_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    /// D-08/03-03: record the source content digest in its own `sha256`
    /// column (migration 0007 — replaces 03-01's `token` placeholder).
    /// Upsert-safe: creates the row when absent, never touches other columns.
    pub async fn set_source_sha256(
        &self,
        vault_id: i64,
        source_id: &str,
        sha256: &str,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO sync_state (vault_id, source_id, sha256) VALUES (?, ?, ?) \
             ON CONFLICT(vault_id, source_id) DO UPDATE SET sha256 = excluded.sha256",
        )
        .bind(vault_id)
        .bind(source_id)
        .bind(sha256)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The dedicated sha256 column (03-04's no-op drift check reads this).
    pub async fn get_source_sha256(
        &self,
        vault_id: i64,
        source_id: &str,
    ) -> Result<Option<String>> {
        let digest = sqlx::query_scalar::<_, Option<String>>(
            "SELECT sha256 FROM sync_state WHERE vault_id = ? AND source_id = ?",
        )
        .bind(vault_id)
        .bind(source_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(digest.flatten())
    }

    /// 04-02: poll bookkeeping — owns ONLY `last_poll_at`/`last_error`. The
    /// poller must never call `upsert_sync_state` (all-column overwrite would
    /// clobber token/etag/sha256). Upsert-safe per `set_source_sha256`.
    pub async fn record_sync_poll(
        &self,
        vault_id: i64,
        source_id: &str,
        last_poll_at: Option<&str>,
        last_error: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO sync_state (vault_id, source_id, last_poll_at, last_error) \
             VALUES (?, ?, ?, ?) \
             ON CONFLICT(vault_id, source_id) DO UPDATE SET \
                last_poll_at = excluded.last_poll_at, \
                last_error = excluded.last_error",
        )
        .bind(vault_id)
        .bind(source_id)
        .bind(last_poll_at)
        .bind(last_error)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 04-02: RFC 6578 sync-collection token — owns ONLY `token`.
    pub async fn set_sync_token(
        &self,
        vault_id: i64,
        source_id: &str,
        token: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO sync_state (vault_id, source_id, token) VALUES (?, ?, ?) \
             ON CONFLICT(vault_id, source_id) DO UPDATE SET token = excluded.token",
        )
        .bind(vault_id)
        .bind(source_id)
        .bind(token)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 04-02 / D-05: the sync-unsupported fallback latch — owns ONLY
    /// `sync_unsupported` (migration 0009).
    pub async fn set_sync_unsupported(
        &self,
        vault_id: i64,
        source_id: &str,
        unsupported: bool,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO sync_state (vault_id, source_id, sync_unsupported) VALUES (?, ?, ?) \
             ON CONFLICT(vault_id, source_id) DO UPDATE SET \
                sync_unsupported = excluded.sync_unsupported",
        )
        .bind(vault_id)
        .bind(source_id)
        .bind(unsupported)
        .execute(&self.pool)
        .await?;
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

    /// sha256 lives in its own column now (migration 0007): set/get round-trip,
    /// re-set overwrites, and an existing row's `token` is left untouched.
    #[tokio::test]
    async fn set_and_get_source_sha256_is_upsert_safe() {
        let store = mem_store().await;
        assert_eq!(store.get_source_sha256(1, "s1").await.unwrap(), None);
        store.set_source_sha256(1, "s1", "aa11").await.unwrap();
        store.set_source_sha256(1, "s1", "bb22").await.unwrap();
        assert_eq!(
            store.get_source_sha256(1, "s1").await.unwrap().as_deref(),
            Some("bb22")
        );
        // Upsert onto a row that already has a token: token survives.
        store
            .upsert_sync_state(1, "s2", Some("tok"), None, None, None)
            .await
            .unwrap();
        store.set_source_sha256(1, "s2", "cc33").await.unwrap();
        let ss = store.get_sync_state(1, "s2").await.unwrap().expect("row");
        assert_eq!(ss.token.as_deref(), Some("tok"));
    }

    /// `record_sync_poll` owns ONLY last_poll_at/last_error: an existing row's
    /// token, etag and sha256 must survive (the poller must never clobber them
    /// via the all-column upsert_sync_state — RESEARCH ⚠️).
    #[tokio::test]
    async fn record_sync_poll_preserves_token_and_sha256() {
        let store = mem_store().await;
        store
            .upsert_sync_state(1, "s1", Some("tok-1"), Some("etag-1"), None, None)
            .await
            .unwrap();
        store.set_source_sha256(1, "s1", "aa11").await.unwrap();

        store
            .record_sync_poll(1, "s1", Some("2026-01-02T00:00:00Z"), None)
            .await
            .unwrap();
        let ss = store.get_sync_state(1, "s1").await.unwrap().expect("row");
        assert_eq!(ss.token.as_deref(), Some("tok-1"));
        assert_eq!(ss.etag.as_deref(), Some("etag-1"));
        assert_eq!(ss.last_poll_at.as_deref(), Some("2026-01-02T00:00:00Z"));
        assert_eq!(ss.last_error, None);
        assert_eq!(store.get_source_sha256(1, "s1").await.unwrap().as_deref(), Some("aa11"));

        // Error path: last_error recorded, everything else still intact.
        store
            .record_sync_poll(1, "s1", Some("2026-01-03T00:00:00Z"), Some("boom"))
            .await
            .unwrap();
        let ss = store.get_sync_state(1, "s1").await.unwrap().expect("row");
        assert_eq!(ss.last_error.as_deref(), Some("boom"));
        assert_eq!(ss.token.as_deref(), Some("tok-1"));

        // Upsert-safe: creates the row when absent.
        store
            .record_sync_poll(1, "s-new", Some("t"), Some("e"))
            .await
            .unwrap();
        let ss = store.get_sync_state(1, "s-new").await.unwrap().expect("row");
        assert_eq!(ss.last_error.as_deref(), Some("e"));
    }

    /// `set_sync_token` owns only `token`; `set_sync_unsupported` owns only
    /// the D-05 latch column. Both upsert-safe, both visible via get_sync_state.
    #[tokio::test]
    async fn set_sync_token_and_flag_roundtrip() {
        let store = mem_store().await;
        store
            .upsert_sync_state(1, "s1", None, Some("etag-1"), Some("p0"), None)
            .await
            .unwrap();

        store.set_sync_token(1, "s1", Some("sync-1")).await.unwrap();
        let ss = store.get_sync_state(1, "s1").await.unwrap().expect("row");
        assert_eq!(ss.token.as_deref(), Some("sync-1"));
        assert_eq!(ss.etag.as_deref(), Some("etag-1")); // untouched
        assert_eq!(ss.last_poll_at.as_deref(), Some("p0")); // untouched

        store.set_sync_token(1, "s1", None).await.unwrap();
        let ss = store.get_sync_state(1, "s1").await.unwrap().expect("row");
        assert_eq!(ss.token, None);

        // sync_unsupported flips 0 → 1 → 0, visible via get_sync_state.
        let ss = store.get_sync_state(1, "s1").await.unwrap().expect("row");
        assert!(!ss.sync_unsupported);
        store.set_sync_unsupported(1, "s1", true).await.unwrap();
        let ss = store.get_sync_state(1, "s1").await.unwrap().expect("row");
        assert!(ss.sync_unsupported);
        assert_eq!(ss.etag.as_deref(), Some("etag-1")); // untouched
        store.set_sync_unsupported(1, "s1", false).await.unwrap();
        let ss = store.get_sync_state(1, "s1").await.unwrap().expect("row");
        assert!(!ss.sync_unsupported);

        // Upsert-safe: creates the row when absent.
        store.set_sync_token(1, "s-new", Some("t9")).await.unwrap();
        let ss = store.get_sync_state(1, "s-new").await.unwrap().expect("row");
        assert_eq!(ss.token.as_deref(), Some("t9"));
    }
}
