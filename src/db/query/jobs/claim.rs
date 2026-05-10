//! Atomic claim of pending jobs (sqlite + postgres backends).

use std::collections::HashMap;

use anyhow::{Result, anyhow};

use crate::core::{JobRun, JobStatus};
use crate::db::query::jobs::count_running_per_slug;
use crate::db::{DbConnection, DbRow, DbValue};

/// Atomically claim up to `limit` pending jobs by setting them to running.
/// Returns the claimed jobs. Respects per-job concurrency limits.
///
/// **Postgres**: Uses `FOR UPDATE SKIP LOCKED` for lock-free atomic claiming
/// across multiple workers. Per-slug concurrency is enforced in the query.
///
/// **SQLite**: Uses SELECT + UPDATE within the caller's IMMEDIATE transaction.
/// SQLite serializes writes, so concurrent workers are safe.
pub fn claim_pending_jobs(
    conn: &dyn DbConnection,
    limit: usize,
    _running_counts: &HashMap<String, i64>,
    job_concurrency: &HashMap<String, u32>,
) -> Result<Vec<JobRun>> {
    if conn.kind() == "postgres" {
        claim_pending_jobs_postgres(conn, limit, job_concurrency)
    } else {
        claim_pending_jobs_sqlite(conn, limit, job_concurrency)
    }
}

/// Postgres: atomic per-slug claiming with `FOR UPDATE SKIP LOCKED`.
fn claim_pending_jobs_postgres(
    conn: &dyn DbConnection,
    limit: usize,
    job_concurrency: &HashMap<String, u32>,
) -> Result<Vec<JobRun>> {
    // Get distinct slugs that have pending jobs
    let slug_rows = conn.query_all(
        "SELECT DISTINCT slug FROM _crap_jobs WHERE status = 'pending'",
        &[],
    )?;

    let mut claimed = Vec::new();

    for slug_row in &slug_rows {
        if claimed.len() >= limit {
            break;
        }

        let Some(slug) = slug_row.opt_text_at(0) else {
            continue;
        };

        let max_conc = job_concurrency.get(&slug).copied().unwrap_or(1) as i64;
        let slots_left = limit - claimed.len();

        // Atomic: claim jobs for this slug where running count is under the limit.
        // FOR UPDATE SKIP LOCKED prevents concurrent workers from claiming the same rows.
        // The running-count subquery is evaluated inside the locked context.
        let now = conn.now_expr();
        let p1 = conn.placeholder(1);
        let p2 = conn.placeholder(2);
        let p3 = conn.placeholder(3);

        let rows = conn.query_all(
            &format!(
                "UPDATE _crap_jobs SET status = 'running', started_at = {now},
                        heartbeat_at = {now}, attempt = attempt + 1
                 WHERE id IN (
                     SELECT id FROM _crap_jobs
                     WHERE status = 'pending' AND slug = {p1}
                       AND (retry_after IS NULL OR retry_after <= {now})
                       AND (SELECT COUNT(*) FROM _crap_jobs
                            WHERE slug = {p1} AND status = 'running') < {p2}
                     ORDER BY created_at ASC
                     LIMIT {p3}
                     FOR UPDATE SKIP LOCKED
                 )
                 RETURNING id, slug, queue, data, attempt, max_attempts,
                           scheduled_by, created_at"
            ),
            &[
                DbValue::Text(slug),
                DbValue::Integer(max_conc),
                DbValue::Integer(slots_left as i64),
            ],
        )?;

        for row in &rows {
            claimed.push(parse_job_row(row)?);
        }
    }

    Ok(claimed)
}

/// SQLite: SELECT + individual UPDATE within an IMMEDIATE transaction.
/// SQLite serializes writes, so concurrent workers are safe.
fn claim_pending_jobs_sqlite(
    conn: &dyn DbConnection,
    limit: usize,
    job_concurrency: &HashMap<String, u32>,
) -> Result<Vec<JobRun>> {
    let now = conn.now_expr();
    let rows = conn.query_all(
        &format!(
            "SELECT id, slug, queue, data, attempt, max_attempts, scheduled_by, created_at
             FROM _crap_jobs
             WHERE status = 'pending'
               AND (retry_after IS NULL OR retry_after <= {now})
             ORDER BY created_at ASC
             LIMIT {}",
            conn.placeholder(1)
        ),
        &[DbValue::Integer((limit * 2) as i64)],
    )?;

    // Get actual running counts from DB (not from caller's stale snapshot)
    let running_counts = count_running_per_slug(conn)?;

    let mut claimed = Vec::new();
    let mut extra_running: HashMap<String, i64> = HashMap::new();

    for row in &rows {
        if claimed.len() >= limit {
            break;
        }

        let Some(id) = row.opt_text_at(0) else {
            continue;
        };
        let Some(slug) = row.opt_text_at(1) else {
            continue;
        };

        // Per-slug concurrency check (DB-sourced + locally tracked)
        let max_conc = job_concurrency.get(&slug).copied().unwrap_or(1) as i64;
        let current = running_counts.get(&slug).copied().unwrap_or(0)
            + extra_running.get(&slug).copied().unwrap_or(0);

        if current >= max_conc {
            continue;
        }

        // Claim the job
        let p1 = conn.placeholder(1);
        let affected = conn.execute(
            &format!(
                "UPDATE _crap_jobs SET status = 'running', started_at = {now},
                        heartbeat_at = {now}, attempt = attempt + 1
                 WHERE id = {p1} AND status = 'pending'"
            ),
            &[DbValue::Text(id.clone())],
        )?;

        if affected > 0 {
            *extra_running.entry(slug).or_insert(0) += 1;
            // SQLite path: the row was SELECTed pre-update, so its
            // `attempt` is the value before the `attempt = attempt + 1`
            // we just executed. Bump the parsed value so the JobRun
            // reflects the post-update DB state, matching what the
            // Postgres `RETURNING` path produces natively.
            let mut job = parse_job_row(row)?;
            job.attempt += 1;
            claimed.push(job);
        }
    }

    Ok(claimed)
}

/// Parse a job row from a DB read into a `JobRun`. The `attempt` value is
/// taken verbatim from the row — callers are responsible for ensuring it
/// reflects "the attempt number now being executed":
///
/// - **Postgres claim path** (`UPDATE ... RETURNING attempt`) returns the
///   post-update value, which is exactly the attempt being executed —
///   pass straight through.
/// - **SQLite claim path** (`SELECT` then `UPDATE`) reads the pre-update
///   value; the caller must `+1` it before constructing the `JobRun` to
///   match the just-incremented DB state.
///
/// Mixing those up was a real bug in earlier alpha.8: a single `+1` in
/// this function plus the SQL increment on the Postgres path produced a
/// double-count, causing jobs to fail one execution earlier than
/// `max_attempts` configures and skewing the retry-backoff index.
fn parse_job_row(row: &DbRow) -> Result<JobRun> {
    let id = row
        .text_at(0)
        .ok_or_else(|| anyhow!("Missing job id"))?
        .to_string();
    let slug = row
        .text_at(1)
        .ok_or_else(|| anyhow!("Missing job slug"))?
        .to_string();
    let queue = row.text_at(2).unwrap_or("default").to_string();
    let data = row.text_at(3).unwrap_or("{}").to_string();
    let attempt = row.i64_at(4).unwrap_or(0) as u32;
    let max_attempts = row.i64_at(5).unwrap_or(1) as u32;

    let mut b = JobRun::builder(id, slug)
        .status(JobStatus::Running)
        .queue(queue)
        .data(data)
        .attempt(attempt)
        .max_attempts(max_attempts);

    if let Some(sb) = row.opt_text_at(6) {
        b = b.scheduled_by(sb);
    }
    if let Some(ca) = row.opt_text_at(7) {
        b = b.created_at(ca);
    }

    Ok(b.build())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::query::jobs::insert_job;
    use crate::db::query::jobs::test_helpers::setup_db;

    #[test]
    fn test_claim_pending_jobs() {
        let (_dir, conn) = setup_db();
        insert_job(&conn, "job_a", "{}", "cron", 1, "default").unwrap();
        insert_job(&conn, "job_b", "{}", "cron", 1, "default").unwrap();

        let running = HashMap::new();
        let conc = HashMap::new();
        let claimed = claim_pending_jobs(&conn, 10, &running, &conc).unwrap();
        assert_eq!(claimed.len(), 2);
        assert_eq!(claimed[0].status, JobStatus::Running);

        // No more pending
        let claimed2 = claim_pending_jobs(&conn, 10, &running, &conc).unwrap();
        assert_eq!(claimed2.len(), 0);
    }

    /// Regression: the SQL claim already does `attempt = attempt + 1` on
    /// the row, and the doc on `fail_job` says the passed `attempt` is
    /// "already incremented by claim". `parse_job_row` previously also
    /// did `.attempt(attempt + 1)`, double-counting — a job inserted with
    /// `attempt = 0` and `max_attempts = 3` would surface as
    /// `JobRun.attempt = 2` after the first claim and reach
    /// `attempt = 3` after the second, hitting the
    /// `attempt < max_attempts` retry threshold one execution early.
    /// Net effect: jobs effectively got `max_attempts - 1` retries and
    /// the failure-backoff delay used the wrong attempt index.
    #[test]
    fn claim_reports_attempt_count_consistent_with_db_increment() {
        let (_dir, conn) = setup_db();
        insert_job(&conn, "retry_test", "{}", "manual", 3, "default").unwrap();

        let running = HashMap::new();
        let conc = HashMap::new();

        // First claim of a fresh job: DB attempt was 0, SQL bumps it to 1,
        // and the parsed JobRun should reflect "this is attempt 1".
        let first = claim_pending_jobs(&conn, 10, &running, &conc).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(
            first[0].attempt, 1,
            "first claim of attempt=0 job must surface as attempt=1, got {}",
            first[0].attempt
        );

        // Simulate a retry: requeue the job (claim_pending_jobs only looks
        // at status='pending', so flip back).
        conn.execute(
            "UPDATE _crap_jobs SET status = 'pending' WHERE id = ?1",
            &[DbValue::Text(first[0].id.clone())],
        )
        .unwrap();

        // Second claim: DB attempt is now 1, SQL bumps to 2, JobRun must
        // surface attempt=2 (NOT 3).
        let second = claim_pending_jobs(&conn, 10, &running, &conc).unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(
            second[0].attempt, 2,
            "second claim of the same job must surface as attempt=2, got {}",
            second[0].attempt
        );
    }

    #[test]
    fn test_claim_respects_concurrency() {
        let (_dir, conn) = setup_db();
        insert_job(&conn, "limited", "{}", "cron", 1, "default").unwrap();
        insert_job(&conn, "limited", "{}", "cron", 1, "default").unwrap();

        let running = HashMap::new();
        let mut conc = HashMap::new();
        conc.insert("limited".to_string(), 1u32);

        let claimed = claim_pending_jobs(&conn, 10, &running, &conc).unwrap();
        assert_eq!(claimed.len(), 1);
    }

    /// Regression: claim_pending_jobs should skip jobs whose retry_after is in the future.
    #[test]
    fn test_claim_skips_jobs_with_future_retry_after() {
        let (_dir, conn) = setup_db();
        insert_job(&conn, "test", "{}", "manual", 3, "default").unwrap();

        // Set retry_after far in the future
        conn.execute(
            "UPDATE _crap_jobs SET retry_after = datetime('now', '+3600 seconds')",
            &[],
        )
        .unwrap();

        let running = HashMap::new();
        let conc = HashMap::new();
        let claimed = claim_pending_jobs(&conn, 10, &running, &conc).unwrap();
        assert_eq!(
            claimed.len(),
            0,
            "should not claim job with future retry_after"
        );
    }

    /// Jobs with retry_after in the past should be claimable.
    #[test]
    fn test_claim_picks_up_jobs_with_past_retry_after() {
        let (_dir, conn) = setup_db();
        insert_job(&conn, "test", "{}", "manual", 3, "default").unwrap();

        // Set retry_after in the past
        conn.execute(
            "UPDATE _crap_jobs SET retry_after = datetime('now', '-10 seconds')",
            &[],
        )
        .unwrap();

        let running = HashMap::new();
        let conc = HashMap::new();
        let claimed = claim_pending_jobs(&conn, 10, &running, &conc).unwrap();
        assert_eq!(claimed.len(), 1, "should claim job with past retry_after");
    }
}
