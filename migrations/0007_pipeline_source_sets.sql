-- 0007_pipeline_source_sets.sql — plan 03-03 (D-17): per-entity source sets
-- + the dedicated sync_state.sha256 column that replaces 03-01's documented
-- `token` placeholder for the source content digest.

ALTER TABLE sync_state ADD COLUMN sha256 TEXT;

-- D-17: "one source" = one distinct source_id per entity. The PK makes a
-- drift re-ingest of the same URL an INSERT OR IGNORE no-op, so it can
-- never double-count toward the 2+ threshold (D-10/D-14).
CREATE TABLE IF NOT EXISTS entity_sources (
    vault_id INTEGER NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
    canonical_name TEXT NOT NULL,
    source_id TEXT NOT NULL,
    first_seen TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (vault_id, canonical_name, source_id)
);

-- Rows written by 03-01's interim token-column scheme must be visible to
-- get_source_sha256, else 03-04's no-op drift check misses them.
UPDATE sync_state SET sha256 = token WHERE sha256 IS NULL AND token IS NOT NULL;
