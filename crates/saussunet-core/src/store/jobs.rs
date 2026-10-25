//! `jobs` table: schema + minimal store APIs only (D-02). Claim/retry
//! behavior lands in Phase 2.

use sqlx::FromRow;

use crate::error::Result;

use super::Store;

#[derive(Debug, Clone, FromRow)]
pub struct Job {
    pub id: i64,
    pub vault_id: i64,
    pub kind: String,
    pub payload_json: String,
    pub status: String,
    pub priority: i64,
    pub attempts: i64,
    pub error: Option<String>,
    pub created_at: String,
    pub claimed_at: Option<String>,
    pub finished_at: Option<String>,
}

impl Store {
    pub async fn insert_job(&self, vault_id: i64, kind: &str, payload_json: &str) -> Result<i64> {
        let res = sqlx::query("INSERT INTO jobs (vault_id, kind, payload_json) VALUES (?, ?, ?)")
            .bind(vault_id)
            .bind(kind)
            .bind(payload_json)
            .execute(&self.pool)
            .await?;
        Ok(res.last_insert_rowid())
    }

    /// List a vault's jobs (VLET-04: vault id on every call). Optional
    /// status filter; claim order (`priority DESC, created_at`) so the
    /// Phase-2 worker reads in the same order it will claim. No claim /
    /// claim-expiry / retry logic here — Phase 2 owns queue behavior (D-02).
    pub async fn list_jobs(&self, vault_id: i64, status: Option<&str>) -> Result<Vec<Job>> {
        let rows =
            match status {
                Some(status) => sqlx::query_as::<_, Job>(
                    "SELECT id, vault_id, kind, payload_json, status, priority, attempts, error, \
                     created_at, claimed_at, finished_at \
                     FROM jobs WHERE vault_id = ? AND status = ? \
                     ORDER BY priority DESC, created_at, id",
                )
                .bind(vault_id)
                .bind(status)
                .fetch_all(&self.pool)
                .await?,
                None => sqlx::query_as::<_, Job>(
                    "SELECT id, vault_id, kind, payload_json, status, priority, attempts, error, \
                     created_at, claimed_at, finished_at \
                     FROM jobs WHERE vault_id = ? \
                     ORDER BY priority DESC, created_at, id",
                )
                .bind(vault_id)
                .fetch_all(&self.pool)
                .await?,
            };
        Ok(rows)
    }
}
