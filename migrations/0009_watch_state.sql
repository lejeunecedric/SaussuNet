-- 0009_watch_state.sql — per-file watcher state for WebDAV sources (SRCC-02).
CREATE TABLE IF NOT EXISTS watch_state (
    vault_id   INTEGER NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
    source_id  TEXT NOT NULL,           -- watch source id (config url, verbatim)
    path       TEXT NOT NULL,           -- decoded path relative to watched root, '/' separators, no leading '/'
    is_dir     INTEGER NOT NULL DEFAULT 0,
    etag       TEXT,
    size       INTEGER,
    sha256     TEXT,                    -- digest of last ingested bytes (D-07 gate)
    last_seen_at TEXT NOT NULL,        -- last poll that observed it (removal detection)
    PRIMARY KEY (vault_id, source_id, path)
);

-- D-05 groundwork: RFC 6578 try-and-fallback latch on sync_state (the latch
-- itself lands in 04-07; this plan only provides the column + its writer).
ALTER TABLE sync_state ADD COLUMN sync_unsupported INTEGER NOT NULL DEFAULT 0;
