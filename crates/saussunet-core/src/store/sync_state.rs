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
}
