//! `vaults` table: the vault registry (D-03/D-04).

use sqlx::FromRow;

use crate::error::{CoreError, Result};

use super::Store;

/// A row of the `vaults` table. Its `id` is the canonical VaultId:
/// `vault_root` is always `data/vaults/<db row id>` (D-04).
#[derive(Debug, Clone, FromRow)]
pub struct VaultRow {
    pub id: i64,
    pub name: String,
    pub created_at: String,
}

impl Store {
    /// Idempotently ensure the configured vault row exists.
    ///
    /// The INSERT is pinned to the configured id via `ON CONFLICT(id)`:
    /// never "SELECT by name then INSERT" — with AUTOINCREMENT, renaming the
    /// vault in config would create row id 2 while `RuntimeConfig.vault_root`
    /// stays `data/vaults/<configured id>`, and paths and rows would drift.
    pub async fn ensure_vault(&self, id: i64, name: &str) -> Result<VaultRow> {
        sqlx::query(
            "INSERT INTO vaults (id, name) VALUES (?, ?) \
             ON CONFLICT(id) DO UPDATE SET name = excluded.name",
        )
        .bind(id)
        .bind(name)
        .execute(&self.pool)
        .await?;
        let row: Option<VaultRow> =
            sqlx::query_as("SELECT id, name, created_at FROM vaults WHERE id = ?")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?;
        match row {
            // Belt-and-braces: the upsert pins the id, so a mismatch means the
            // DB is in a state we do not understand — fail boot loudly.
            Some(v) if v.id == id => Ok(v),
            Some(v) => Err(CoreError::Vault(format!(
                "ensure_vault: asked for vault id {id} but row has id {} — refusing to continue (D-04)"
                , v.id
            ))),
            None => Err(CoreError::Vault(format!(
                "ensure_vault: upsert of vault id {id} left no row — DB inconsistent"
            ))),
        }
    }
}
