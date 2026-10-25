-- 0004_pending_proposals.sql — conflict recorder (D-06/D-07): the writer
-- records refused writes here for the Phase 5 review queue.
--
-- D-01 exception note: `proposed_content` is durable proposal state — the
-- proposed page body exists nowhere else (the writer skipped the write, so it
-- never landed in the vault). D-01's rebuildability rule applies to
-- vault-DERIVED tables (registries); this table is not derived from vault
-- content, it is the record of a write that was refused.

CREATE TABLE IF NOT EXISTS pending_proposals (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    vault_id INTEGER NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
    target_path TEXT NOT NULL,
    reason TEXT NOT NULL CHECK (reason IN ('gen_hash_mismatch', 'protected', 'human_owned')),
    proposed_content TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
