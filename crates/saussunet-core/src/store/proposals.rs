//! `pending_proposals` table: conflict recorder (D-06/D-07). Insert-only in
//! Phase 1; the Phase 5 review queue consumes the rows.

use serde::Serialize;
use sqlx::FromRow;

use crate::error::Result;

use super::Store;

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct Proposal {
    pub id: i64,
    pub vault_id: i64,
    pub target_path: String,
    pub reason: String,
    pub proposed_content: String,
    pub status: String,
    pub created_at: String,
}

/// Lightweight listing row — `proposed_content` is deliberately excluded so
/// the human-readable `conflicts` output never dumps page bodies; the
/// `--json` path serves full [`Proposal`] rows instead.
#[derive(Debug, Clone, FromRow, Serialize)]
pub struct ProposalSummary {
    pub id: i64,
    pub target_path: String,
    pub reason: String,
    pub created_at: String,
}

impl Store {
    /// Record a refused write as a conflict row. `reason` must be one of
    /// `gen_hash_mismatch`, `protected`, `human_owned` (CHECK constraint).
    pub async fn insert_proposal(
        &self,
        vault_id: i64,
        target_path: &str,
        reason: &str,
        proposed_content: &str,
    ) -> Result<i64> {
        let res = sqlx::query(
            "INSERT INTO pending_proposals (vault_id, target_path, reason, proposed_content) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(vault_id)
        .bind(target_path)
        .bind(reason)
        .bind(proposed_content)
        .execute(&self.pool)
        .await?;
        Ok(res.last_insert_rowid())
    }

    pub async fn list_pending_proposals(&self, vault_id: i64) -> Result<Vec<Proposal>> {
        let rows = sqlx::query_as::<_, Proposal>(
            "SELECT id, vault_id, target_path, reason, proposed_content, status, created_at \
             FROM pending_proposals WHERE vault_id = ? AND status = 'pending' ORDER BY id",
        )
        .bind(vault_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Pending rows without `proposed_content` — the `conflicts` CLI's
    /// human-readable listing (content only ever surfaces via `--json`).
    pub async fn list_pending_summary(&self, vault_id: i64) -> Result<Vec<ProposalSummary>> {
        let rows = sqlx::query_as::<_, ProposalSummary>(
            "SELECT id, target_path, reason, created_at \
             FROM pending_proposals WHERE vault_id = ? AND status = 'pending' ORDER BY id",
        )
        .bind(vault_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    pub async fn count_pending(&self, vault_id: i64) -> Result<i64> {
        let (n,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM pending_proposals WHERE vault_id = ? AND status = 'pending'",
        )
        .bind(vault_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(n)
    }
}
