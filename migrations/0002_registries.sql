-- 0002_registries.sql — rebuildable projections of the vault filesystem.
-- D-01: SQLite NEVER stores note content; these tables are pure projections
-- rebuilt by `reindex`. D-03: every table carries vault_id.

CREATE TABLE IF NOT EXISTS page_registry (
    vault_id INTEGER NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    title TEXT NOT NULL,
    page_type TEXT NOT NULL,
    tags_json TEXT NOT NULL DEFAULT '[]',
    updated TEXT,
    has_gen_hash INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (vault_id, path)
);

CREATE TABLE IF NOT EXISTS entity_registry (
    vault_id INTEGER NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
    canonical_name TEXT NOT NULL,
    aliases_json TEXT NOT NULL DEFAULT '[]',
    page_path TEXT,
    page_type TEXT,
    PRIMARY KEY (vault_id, canonical_name)
);
