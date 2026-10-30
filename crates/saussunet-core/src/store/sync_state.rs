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
            "SELECT vault_id, source_id, token, etag, last_poll_at, last_error \
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
}
