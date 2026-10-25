-- 0003_jobs_sync_state.sql — queue + watcher state (D-02: schema created in
-- Phase 1, behavior lands in Phases 2/4). D-03: vault_id on every table.

CREATE TABLE IF NOT EXISTS jobs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    vault_id INTEGER NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    payload_json TEXT NOT NULL DEFAULT '{}',
    status TEXT NOT NULL DEFAULT 'pending',
    priority INTEGER NOT NULL DEFAULT 0,
    attempts INTEGER NOT NULL DEFAULT 0,
    error TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    claimed_at TEXT,
    finished_at TEXT
);

-- Claim-order index for the Phase 2 queue worker (vault_id, status, priority, age).
CREATE INDEX IF NOT EXISTS idx_jobs_claim ON jobs(vault_id, status, priority, created_at);

CREATE TABLE IF NOT EXISTS sync_state (
    vault_id INTEGER NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
    source_id TEXT NOT NULL,
    token TEXT,
    etag TEXT,
    last_poll_at TEXT,
    last_error TEXT,
    PRIMARY KEY (vault_id, source_id)
);
