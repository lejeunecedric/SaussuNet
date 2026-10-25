//! `entity_registry` table: rebuildable projection of canonical entity names.

use sqlx::FromRow;

use crate::error::Result;

use super::Store;

#[derive(Debug, Clone, FromRow)]
pub struct EntityRecord {
    pub vault_id: i64,
    pub canonical_name: String,
    pub aliases_json: String,
    pub page_path: Option<String>,
    pub page_type: Option<String>,
}

impl Store {
    pub async fn upsert_entity(
        &self,
        vault_id: i64,
        canonical_name: &str,
        aliases_json: &str,
        page_path: Option<&str>,
        page_type: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO entity_registry \
                (vault_id, canonical_name, aliases_json, page_path, page_type) \
             VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT(vault_id, canonical_name) DO UPDATE SET \
                aliases_json = excluded.aliases_json, \
                page_path = excluded.page_path, \
                page_type = excluded.page_type",
        )
        .bind(vault_id)
        .bind(canonical_name)
        .bind(aliases_json)
        .bind(page_path)
        .bind(page_type)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_entities(&self, vault_id: i64) -> Result<Vec<EntityRecord>> {
        let rows = sqlx::query_as::<_, EntityRecord>(
            "SELECT vault_id, canonical_name, aliases_json, page_path, page_type \
             FROM entity_registry WHERE vault_id = ? ORDER BY canonical_name",
        )
        .bind(vault_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }
}
