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

    /// Resolve-before-create lookup (Phase 3 contract, Pitfall 2): exact
    /// match of the NORMALIZED name (lowercase, trimmed, whitespace
    /// collapsed) against `canonical_name` OR any entry in `aliases_json`.
    /// Candidates are fetched for the vault and matched in Rust —
    /// personal-scale registries need no LIKE/index tricks. Fuzzy /
    /// threshold / LLM matching is explicitly NOT here (Phase 3 owns it).
    pub async fn resolve_entity(&self, vault_id: i64, name: &str) -> Result<Option<EntityRecord>> {
        let target = normalize_name(name);
        if target.is_empty() {
            return Ok(None);
        }
        for row in self.list_entities(vault_id).await? {
            if normalize_name(&row.canonical_name) == target {
                return Ok(Some(row));
            }
            let aliases: Vec<String> = serde_json::from_str(&row.aliases_json).unwrap_or_default();
            if aliases.iter().any(|a| normalize_name(a) == target) {
                return Ok(Some(row));
            }
        }
        Ok(None)
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

/// Name normalization shared by resolve-before-create: lowercase, trim,
/// collapse internal whitespace runs to a single space.
fn normalize_name(name: &str) -> String {
    name.trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
