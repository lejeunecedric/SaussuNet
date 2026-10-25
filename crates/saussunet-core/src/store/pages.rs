//! `page_registry` table: rebuildable projection of vault pages (D-01 — no
//! body/content column, ever). Upserts use `INSERT ... ON CONFLICT` per
//! PATTERNS §4.8 (cleaner than MoriNet's SELECT-then-INSERT under the
//! single-writer rule).

use sqlx::FromRow;

use crate::error::Result;

use super::Store;

#[derive(Debug, Clone, FromRow)]
pub struct PageRecord {
    pub vault_id: i64,
    pub path: String,
    pub title: String,
    pub page_type: String,
    pub tags_json: String,
    pub updated: Option<String>,
    pub has_gen_hash: bool,
}

impl Store {
    #[allow(clippy::too_many_arguments)]
    pub async fn upsert_page(
        &self,
        vault_id: i64,
        path: &str,
        title: &str,
        page_type: &str,
        tags_json: &str,
        updated: Option<&str>,
        has_gen_hash: bool,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO page_registry \
                (vault_id, path, title, page_type, tags_json, updated, has_gen_hash) \
             VALUES (?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(vault_id, path) DO UPDATE SET \
                title = excluded.title, \
                page_type = excluded.page_type, \
                tags_json = excluded.tags_json, \
                updated = excluded.updated, \
                has_gen_hash = excluded.has_gen_hash",
        )
        .bind(vault_id)
        .bind(path)
        .bind(title)
        .bind(page_type)
        .bind(tags_json)
        .bind(updated)
        .bind(has_gen_hash)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_pages(&self, vault_id: i64) -> Result<Vec<PageRecord>> {
        let rows = sqlx::query_as::<_, PageRecord>(
            "SELECT vault_id, path, title, page_type, tags_json, updated, has_gen_hash \
             FROM page_registry WHERE vault_id = ? ORDER BY path",
        )
        .bind(vault_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Drop all projections for a vault (used by `reindex` in a later plan).
    pub async fn clear_pages(&self, vault_id: i64) -> Result<u64> {
        let res = sqlx::query("DELETE FROM page_registry WHERE vault_id = ?")
            .bind(vault_id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected())
    }
}
