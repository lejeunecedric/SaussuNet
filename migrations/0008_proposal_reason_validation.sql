-- 0008_proposal_reason_validation.sql — PIPE-06: the deterministic write-path
-- validator records rejected generated pages in pending_proposals (reason
-- 'validation_rejected') so the Phase 5 review queue sees what never reached
-- disk. SQLite cannot ALTER a CHECK constraint → standard rebuild dance.
-- pending_proposals is a child table (nothing references it), so foreign
-- keys stay consistent across the swap without the PRAGMA toggle.

CREATE TABLE pending_proposals_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    vault_id INTEGER NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
    target_path TEXT NOT NULL,
    reason TEXT NOT NULL CHECK (reason IN ('gen_hash_mismatch', 'protected', 'human_owned', 'validation_rejected')),
    proposed_content TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

INSERT INTO pending_proposals_new (id, vault_id, target_path, reason, proposed_content, status, created_at)
    SELECT id, vault_id, target_path, reason, proposed_content, status, created_at
    FROM pending_proposals;

DROP TABLE pending_proposals;

ALTER TABLE pending_proposals_new RENAME TO pending_proposals;
