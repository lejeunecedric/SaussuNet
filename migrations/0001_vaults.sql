-- 0001_vaults.sql — the vault registry (D-03/D-04: one DB, every other table
-- carries vault_id and references this table).
CREATE TABLE IF NOT EXISTS vaults (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
