-- 0005_jobs_next_attempt_at.sql — retry due-time gate for D-09 backoff.
-- DOCUMENTED EXCEPTION to Phase 1 D-02 ("design the schema once in Phase 1",
-- not "no migrations ever"): retry due-time gating needs a dedicated column.
-- Reusing claimed_at as the "not before" marker was rejected: D-11 heartbeats
-- overwrite claimed_at every 60 s, which would corrupt any claimed_at-based
-- due math and make reap-vs-backoff indistinguishable.

ALTER TABLE jobs ADD COLUMN next_attempt_at TEXT;
