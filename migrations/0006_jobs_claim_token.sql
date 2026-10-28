-- 0006_jobs_claim_token.sql — claim fencing token (WR-01).
-- Every claim mints a fresh random token; complete/fail/touch transitions
-- only succeed for the CURRENT claim. A stale worker whose lease was reaped
-- and re-claimed elsewhere carries a dead token and can no longer clobber
-- the live claim. `claimed_at` cannot serve as the token because D-11
-- heartbeats overwrite it every 60 s (same reasoning as 0005 rejecting
-- claimed_at reuse for backoff math).

ALTER TABLE jobs ADD COLUMN claim_token TEXT;
