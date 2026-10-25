//! SQLite store: WAL pool init + migrations (PATTERNS §2.1 — MoriNet's
//! `store.rs:84-111` shape upgraded with the pragmas MoriNet is missing).
//! Per-table APIs live in the submodules below (§2.3 idiom: typed row structs,
//! `.bind()` chains, `fetch_optional`). Every query is parameterized — no
//! string-formatted SQL anywhere (T-01-03).

mod entities;
mod jobs;
mod pages;
mod proposals;
mod sync_state;
mod vaults;

pub use proposals::{Proposal, ProposalSummary};
pub use vaults::VaultRow;

use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::SqlitePool;

use crate::error::Result;

pub struct Store {
    pool: SqlitePool,
}

impl Store {
    /// Open (or create) the SQLite database at `db_path` and run migrations.
    /// WAL + busy_timeout + foreign_keys per INFR-02 / PATTERNS §4.2: FKs are
    /// declared in migrations but SQLite defaults enforcement OFF per
    /// connection, and `ON DELETE CASCADE` (D-03) is dead without them.
    pub async fn connect(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| crate::error::CoreError::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }
        let opts =
            SqliteConnectOptions::from_str(&format!("sqlite://{}?mode=rwc", db_path.display()))?
                .create_if_missing(true)
                .journal_mode(SqliteJournalMode::Wal)
                .busy_timeout(Duration::from_secs(5))
                .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(opts)
            .await?;
        // PATTERNS §4.5 trap: sqlx::migrate! resolves relative to the crate
        // that calls it; migrations/ lives at the WORKSPACE root, so the path
        // is ../../migrations from crates/saussunet-core. A test
        // (tests/init_vault.rs) asserts all six tables land, so a wrong path
        // can never silently embed nothing.
        run_migrations(&pool).await?;
        Ok(Self { pool })
    }

    /// In-memory store for tests (MoriNet store.rs:104-111 idiom). Same
    /// migrations; WAL omitted — `:memory:` databases cannot use it.
    pub async fn connect_memory() -> Result<Self> {
        let opts = SqliteConnectOptions::from_str("sqlite::memory:")?
            .create_if_missing(true)
            .busy_timeout(Duration::from_secs(5))
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(opts)
            .await?;
        run_migrations(&pool).await?;
        Ok(Self { pool })
    }

    /// Escape hatch for tests and later phases (transactions, raw reads).
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

/// Run the embedded workspace migrations. A failed migration aborts boot
/// loudly (T-01-05: no partial-schema silent operation). sqlx 0.8 returns
/// `MigrateError` (not `sqlx::Error`) here, hence the explicit conversion.
async fn run_migrations(pool: &SqlitePool) -> Result<()> {
    sqlx::migrate!("../../migrations")
        .run(pool)
        .await
        .map_err(|e| crate::error::CoreError::Store(sqlx::Error::Migrate(Box::new(e))))
}
