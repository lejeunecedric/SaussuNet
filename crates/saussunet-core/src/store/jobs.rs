//! `jobs` table: full durable queue lifecycle (Phase 2). A row lives
//! insert → atomic claim → done, or fail → backoff-gated retry → dead.
//! All time comparisons take an injected `now` (planner decision:
//! `tokio::time::pause()` does NOT advance SQLite's clock, so injected-clock
//! params are the only deterministic test path; production passes `Utc::now()`).

use chrono::{DateTime, Duration, Utc};
use sqlx::FromRow;

use crate::error::Result;

use super::Store;

/// D-09: 3 retries with exponential backoff, then dead-letter (D-10).
pub const MAX_RETRIES: i64 = 3;
/// D-09 verbatim: 30 s / 2 min / 10 min for retries 1 / 2 / 3.
pub const BACKOFF_SECS: [i64; 3] = [30, 120, 600];

/// SQLite `datetime('now')` wire format — every timestamp column is TEXT
/// in exactly this format, so string comparison == chronological comparison.
fn ts(now: DateTime<Utc>) -> String {
    now.format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Outcome of `fail_job`, so the worker can log/react without re-querying.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobOutcome {
    /// Backed off to `pending`; due again at `next_attempt_at` (SQLite format).
    Retrying { next_attempt_at: String },
    /// Past `MAX_RETRIES`: `status='dead'`, error retained for post-mortem (D-10).
    Dead,
    /// WR-01 fencing: the caller's `claim_token` no longer matches the row's
    /// current claim (lease was reaped and/or re-claimed elsewhere) — the
    /// transition was rejected, the live claim is untouched.
    ClaimLost,
}

#[derive(Debug, Clone, FromRow)]
pub struct Job {
    pub id: i64,
    pub vault_id: i64,
    pub kind: String,
    pub payload_json: String,
    pub status: String,
    pub priority: i64,
    pub attempts: i64,
    pub error: Option<String>,
    pub created_at: String,
    pub claimed_at: Option<String>,
    pub finished_at: Option<String>,
    /// WR-01 fencing token: minted fresh by `claim_jobs`, cleared on reap.
    /// `complete_job` / `fail_job` / `touch_claim` only succeed while the
    /// caller presents the token of the CURRENT claim.
    pub claim_token: Option<String>,
}

impl Store {
    pub async fn insert_job(&self, vault_id: i64, kind: &str, payload_json: &str) -> Result<i64> {
        let res = sqlx::query("INSERT INTO jobs (vault_id, kind, payload_json) VALUES (?, ?, ?)")
            .bind(vault_id)
            .bind(kind)
            .bind(payload_json)
            .execute(&self.pool)
            .await?;
        Ok(res.last_insert_rowid())
    }

    /// List a vault's jobs (VLET-04: vault id on every call). Optional
    /// status filter; claim order (`priority DESC, created_at`) so the
    /// Phase-2 worker reads in the same order it will claim. No claim /
    /// claim-expiry / retry logic here — Phase 2 owns queue behavior (D-02).
    pub async fn list_jobs(&self, vault_id: i64, status: Option<&str>) -> Result<Vec<Job>> {
        let rows =
            match status {
                Some(status) => sqlx::query_as::<_, Job>(
                    "SELECT id, vault_id, kind, payload_json, status, priority, attempts, error, \
                     created_at, claimed_at, finished_at, claim_token \
                     FROM jobs WHERE vault_id = ? AND status = ? \
                     ORDER BY priority DESC, created_at, id",
                )
                .bind(vault_id)
                .bind(status)
                .fetch_all(&self.pool)
                .await?,
                None => sqlx::query_as::<_, Job>(
                    "SELECT id, vault_id, kind, payload_json, status, priority, attempts, error, \
                     created_at, claimed_at, finished_at, claim_token \
                     FROM jobs WHERE vault_id = ? \
                     ORDER BY priority DESC, created_at, id",
                )
                .bind(vault_id)
                .fetch_all(&self.pool)
                .await?,
            };
        Ok(rows)
    }

    /// D-12: atomically move at most `n` due pending jobs to `claimed`.
    /// Single transaction (SQLite serializes writers; `busy_timeout(5s)`
    /// handles contention), so N concurrent claimers never see the same row.
    /// Due gate: a pending job with `next_attempt_at` in the future is NOT
    /// claimable (D-09 backoff). Same `ORDER BY` as `list_jobs` so the claim
    /// order matches `idx_jobs_claim`.
    pub async fn claim_jobs(&self, vault_id: i64, n: i64, now: DateTime<Utc>) -> Result<Vec<Job>> {
        let now_s = ts(now);
        let mut tx = self.pool.begin().await?;
        let rows = sqlx::query_as::<_, Job>(
            "UPDATE jobs SET status='claimed', claimed_at=?, \
             claim_token=lower(hex(randomblob(16))) \
             WHERE id IN (SELECT id FROM jobs \
                          WHERE status='pending' AND vault_id=? \
                            AND (next_attempt_at IS NULL OR next_attempt_at <= ?) \
                          ORDER BY priority DESC, created_at, id LIMIT ?) \
             RETURNING id, vault_id, kind, payload_json, status, priority, \
                       attempts, error, created_at, claimed_at, finished_at, claim_token",
        )
        .bind(&now_s)
        .bind(vault_id)
        .bind(&now_s)
        .bind(n)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(rows)
    }

    /// D-11 heartbeat: refresh `claimed_at` on a live claim so the reaper
    /// leaves it alone. WR-01: fenced on `claim_token` — a stale claimant
    /// cannot extend someone else's live lease. Returns rows affected (0 if
    /// the row is no longer claimed *by this caller* — e.g. reaped,
    /// completed, or re-claimed elsewhere).
    pub async fn touch_claim(
        &self,
        job_id: i64,
        claim_token: &str,
        now: DateTime<Utc>,
    ) -> Result<u64> {
        let res = sqlx::query(
            "UPDATE jobs SET claimed_at=? WHERE id=? AND status='claimed' AND claim_token=?",
        )
        .bind(ts(now))
        .bind(job_id)
        .bind(claim_token)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }

    /// Claimed → done with `finished_at`. WR-01: fenced on `claim_token` —
    /// only the holder of the CURRENT claim may complete; a stale claimant
    /// whose lease was reaped and re-claimed elsewhere affects 0 rows.
    /// Returns rows affected (0 if the row is not claimed by this token).
    pub async fn complete_job(
        &self,
        job_id: i64,
        claim_token: &str,
        now: DateTime<Utc>,
    ) -> Result<u64> {
        let res = sqlx::query(
            "UPDATE jobs SET status='done', finished_at=?, claim_token=NULL \
             WHERE id=? AND status='claimed' AND claim_token=?",
        )
        .bind(ts(now))
        .bind(job_id)
        .bind(claim_token)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }

    /// D-09/D-10: attempts++ → back to `pending` with
    /// `next_attempt_at = now + 30s / +2min / +10min` for retries 1/2/3;
    /// the 4th failure sets `status='dead'` and retains the error text.
    /// WR-01: fenced on `claim_token` — the SELECT + UPDATE run in one
    /// transaction and both carry the `status='claimed' AND claim_token=?`
    /// guard, so a stale claimant cannot clobber a live claim (returns
    /// [`JobOutcome::ClaimLost`] and touches nothing).
    pub async fn fail_job(
        &self,
        job_id: i64,
        claim_token: &str,
        error: &str,
        now: DateTime<Utc>,
    ) -> Result<JobOutcome> {
        let now_s = ts(now);
        let mut tx = self.pool.begin().await?;
        let attempts: Option<i64> = sqlx::query_scalar(
            "SELECT attempts FROM jobs WHERE id=? AND status='claimed' AND claim_token=?",
        )
        .bind(job_id)
        .bind(claim_token)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(attempts) = attempts else {
            tx.commit().await?;
            return Ok(JobOutcome::ClaimLost);
        };
        let new_attempts = attempts + 1;
        if new_attempts > MAX_RETRIES {
            sqlx::query(
                "UPDATE jobs SET status='dead', error=?, finished_at=?, attempts=? \
                 WHERE id=? AND status='claimed' AND claim_token=?",
            )
            .bind(error)
            .bind(&now_s)
            .bind(new_attempts)
            .bind(job_id)
            .bind(claim_token)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            Ok(JobOutcome::Dead)
        } else {
            let next = now + Duration::seconds(BACKOFF_SECS[(new_attempts - 1) as usize]);
            let next_s = ts(next);
            sqlx::query(
                "UPDATE jobs SET status='pending', error=?, attempts=?, claimed_at=NULL, \
                 claim_token=NULL, next_attempt_at=? \
                 WHERE id=? AND status='claimed' AND claim_token=?",
            )
            .bind(error)
            .bind(new_attempts)
            .bind(&next_s)
            .bind(job_id)
            .bind(claim_token)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            Ok(JobOutcome::Retrying {
                next_attempt_at: next_s,
            })
        }
    }

    /// D-11 (expiry half): return claimed jobs whose `claimed_at` is older
    /// than the expiry window to `pending`. `attempts` is preserved so a
    /// crashed worker's retries still count toward D-09. Done/dead rows are
    /// never touched. Returns rows reaped.
    pub async fn reap_expired_claims(
        &self,
        vault_id: i64,
        expiry: Duration,
        now: DateTime<Utc>,
    ) -> Result<u64> {
        let cutoff = ts(now - expiry);
        let res = sqlx::query(
            "UPDATE jobs SET status='pending', claimed_at=NULL, claim_token=NULL \
             WHERE status='claimed' AND vault_id=? AND claimed_at < ?",
        )
        .bind(vault_id)
        .bind(cutoff)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }
}
