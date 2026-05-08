//! Job lifecycle writes: insert, complete, fail (with retry backoff),
//! heartbeat, mark-stale.

use std::cmp;

use anyhow::{Context as _, Result};
use nanoid::nanoid;

use crate::core::job::JobRun;
use crate::db::{DbConnection, DbValue};

/// Insert a new pending job run.
pub fn insert_job(
    conn: &dyn DbConnection,
    slug: &str,
    data: &str,
    scheduled_by: &str,
    max_attempts: u32,
    queue: &str,
) -> Result<JobRun> {
    let id = nanoid!();
    let (p1, p2, p3, p4, p5, p6) = (
        conn.placeholder(1),
        conn.placeholder(2),
        conn.placeholder(3),
        conn.placeholder(4),
        conn.placeholder(5),
        conn.placeholder(6),
    );

    conn.execute(
        &format!(
            "INSERT INTO _crap_jobs (id, slug, status, queue, data, max_attempts, scheduled_by)
         VALUES ({p1}, {p2}, 'pending', {p3}, {p4}, {p5}, {p6})"
        ),
        &[
            DbValue::Text(id.clone()),
            DbValue::Text(slug.to_string()),
            DbValue::Text(queue.to_string()),
            DbValue::Text(data.to_string()),
            DbValue::Integer(max_attempts as i64),
            DbValue::Text(scheduled_by.to_string()),
        ],
    )
    .context("Failed to insert job run")?;

    Ok(JobRun::builder(id, slug)
        .queue(queue)
        .data(data)
        .max_attempts(max_attempts)
        .scheduled_by(scheduled_by)
        .build())
}

/// Mark a job as completed with an optional result.
pub fn complete_job(conn: &dyn DbConnection, id: &str, result_json: Option<&str>) -> Result<()> {
    let result_val = match result_json {
        Some(r) => DbValue::Text(r.to_string()),
        None => DbValue::Null,
    };
    let (p1, p2) = (conn.placeholder(1), conn.placeholder(2));

    conn.execute(
        &format!(
            "UPDATE _crap_jobs SET status = 'completed', result = {p2}, completed_at = {}
         WHERE id = {p1}",
            conn.now_expr()
        ),
        &[DbValue::Text(id.to_string()), result_val],
    )
    .context("Failed to complete job")?;

    Ok(())
}

/// Compute exponential backoff delay in seconds for a given attempt number.
///
/// Formula: `min(2^(attempt-1) * 5, 300)` — yields 5s, 10s, 20s, 40s, 80s, 160s, 300s cap.
/// `attempt` is 1-based (first failure = attempt 1).
fn backoff_seconds(attempt: u32) -> i64 {
    let exp = attempt.saturating_sub(1).min(6) as i64;

    cmp::min(5 * (1i64 << exp), 300)
}

/// Mark a job as failed. If should_retry is true, resets to pending with exponential backoff.
/// `attempt` is the current attempt number (already incremented by claim).
pub fn fail_job(
    conn: &dyn DbConnection,
    id: &str,
    error: &str,
    should_retry: bool,
    attempt: u32,
) -> Result<()> {
    let (p1, p2) = (conn.placeholder(1), conn.placeholder(2));

    if should_retry {
        let delay = backoff_seconds(attempt);

        // Use the date_offset_expr SQL template but with a positive offset (future time).
        // date_offset_expr returns e.g. ("datetime('now', ?3)", _) — we override the
        // param to "+N seconds" instead of the default "-N seconds".
        let (offset_sql, _) = conn.date_offset_expr(delay, 3);
        let offset_param = DbValue::Text(format!("+{} seconds", delay));

        conn.execute(
            &format!(
                "UPDATE _crap_jobs SET status = 'pending', error = {p2}, \
                 started_at = NULL, completed_at = NULL, heartbeat_at = NULL, \
                 retry_after = {offset_sql} \
                 WHERE id = {p1}"
            ),
            &[
                DbValue::Text(id.to_string()),
                DbValue::Text(error.to_string()),
                offset_param,
            ],
        )
        .context("Failed to retry job")?;
    } else {
        conn.execute(
            &format!(
                "UPDATE _crap_jobs SET status = 'failed', error = {p2}, completed_at = {}
             WHERE id = {p1}",
                conn.now_expr()
            ),
            &[
                DbValue::Text(id.to_string()),
                DbValue::Text(error.to_string()),
            ],
        )
        .context("Failed to fail job")?;
    }

    Ok(())
}

/// Update the heartbeat timestamp for a running job.
pub fn update_heartbeat(conn: &dyn DbConnection, id: &str) -> Result<()> {
    conn.execute(
        &format!(
            "UPDATE _crap_jobs SET heartbeat_at = {} WHERE id = {}",
            conn.now_expr(),
            conn.placeholder(1)
        ),
        &[DbValue::Text(id.to_string())],
    )
    .context("Failed to update heartbeat")?;

    Ok(())
}

/// Mark a running job as stale.
pub fn mark_stale(conn: &dyn DbConnection, id: &str, error: &str) -> Result<()> {
    let (p1, p2) = (conn.placeholder(1), conn.placeholder(2));
    conn.execute(
        &format!(
            "UPDATE _crap_jobs SET status = 'stale', error = {p2}, completed_at = {}
         WHERE id = {p1}",
            conn.now_expr()
        ),
        &[
            DbValue::Text(id.to_string()),
            DbValue::Text(error.to_string()),
        ],
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::job::JobStatus;
    use crate::db::query::jobs::get_job_run;
    use crate::db::query::jobs::test_helpers::setup_db;

    #[test]
    fn test_insert_and_get_job() {
        let (_dir, conn) = setup_db();
        let job = insert_job(&conn, "test_job", "{}", "manual", 1, "default").unwrap();
        assert_eq!(job.slug, "test_job");
        assert_eq!(job.status, JobStatus::Pending);

        let fetched = get_job_run(&conn, &job.id).unwrap().unwrap();
        assert_eq!(fetched.slug, "test_job");
        assert_eq!(fetched.status, JobStatus::Pending);
    }

    #[test]
    fn test_complete_job() {
        let (_dir, conn) = setup_db();
        let job = insert_job(&conn, "test", "{}", "manual", 1, "default").unwrap();
        // Claim it first
        conn.execute(
            "UPDATE _crap_jobs SET status = 'running' WHERE id = ?1",
            &[DbValue::Text(job.id.clone())],
        )
        .unwrap();

        complete_job(&conn, &job.id, Some("{\"ok\":true}")).unwrap();
        let fetched = get_job_run(&conn, &job.id).unwrap().unwrap();
        assert_eq!(fetched.status, JobStatus::Completed);
        assert_eq!(fetched.result.as_deref(), Some("{\"ok\":true}"));
    }

    #[test]
    fn test_fail_job_no_retry() {
        let (_dir, conn) = setup_db();
        let job = insert_job(&conn, "test", "{}", "manual", 1, "default").unwrap();
        conn.execute(
            "UPDATE _crap_jobs SET status = 'running' WHERE id = ?1",
            &[DbValue::Text(job.id.clone())],
        )
        .unwrap();

        fail_job(&conn, &job.id, "something broke", false, 1).unwrap();
        let fetched = get_job_run(&conn, &job.id).unwrap().unwrap();
        assert_eq!(fetched.status, JobStatus::Failed);
        assert_eq!(fetched.error.as_deref(), Some("something broke"));
    }

    #[test]
    fn test_fail_job_with_retry() {
        let (_dir, conn) = setup_db();
        let job = insert_job(&conn, "test", "{}", "manual", 3, "default").unwrap();
        conn.execute(
            "UPDATE _crap_jobs SET status = 'running', attempt = 1 WHERE id = ?1",
            &[DbValue::Text(job.id.clone())],
        )
        .unwrap();

        fail_job(&conn, &job.id, "transient error", true, 1).unwrap();
        let fetched = get_job_run(&conn, &job.id).unwrap().unwrap();
        assert_eq!(fetched.status, JobStatus::Pending);
    }

    /// Regression: fail_job with retry did not clear heartbeat_at, causing the
    /// re-queued job to be immediately detected as stale by find_stale_jobs.
    #[test]
    fn test_fail_job_retry_clears_heartbeat() {
        let (_dir, conn) = setup_db();
        let job = insert_job(&conn, "test", "{}", "manual", 3, "default").unwrap();
        conn.execute(
            "UPDATE _crap_jobs SET status = 'running', attempt = 1, heartbeat_at = datetime('now') WHERE id = ?1",
            &[DbValue::Text(job.id.clone())],
        )
        .unwrap();

        fail_job(&conn, &job.id, "transient error", true, 1).unwrap();
        let fetched = get_job_run(&conn, &job.id).unwrap().unwrap();
        assert_eq!(fetched.status, JobStatus::Pending);
        assert!(
            fetched.heartbeat_at.is_none(),
            "heartbeat_at should be cleared on retry so the job is not detected as stale"
        );
    }

    #[test]
    fn test_mark_stale() {
        let (_dir, conn) = setup_db();
        let job = insert_job(&conn, "test", "{}", "manual", 1, "default").unwrap();
        conn.execute(
            "UPDATE _crap_jobs SET status = 'running' WHERE id = ?1",
            &[DbValue::Text(job.id.clone())],
        )
        .unwrap();

        mark_stale(&conn, &job.id, "heartbeat timeout").unwrap();
        let fetched = get_job_run(&conn, &job.id).unwrap().unwrap();
        assert_eq!(fetched.status, JobStatus::Stale);
        assert_eq!(fetched.error.as_deref(), Some("heartbeat timeout"));
    }

    #[test]
    fn test_update_heartbeat() {
        let (_dir, conn) = setup_db();
        let job = insert_job(&conn, "test", "{}", "manual", 1, "default").unwrap();
        conn.execute(
            "UPDATE _crap_jobs SET status = 'running' WHERE id = ?1",
            &[DbValue::Text(job.id.clone())],
        )
        .unwrap();

        // Update heartbeat should succeed
        update_heartbeat(&conn, &job.id).unwrap();

        let fetched = get_job_run(&conn, &job.id).unwrap().unwrap();
        assert!(
            fetched.heartbeat_at.is_some(),
            "heartbeat should be set after update"
        );
    }

    #[test]
    fn test_backoff_seconds() {
        // attempt is 1-based (first failure = 1 after claim increments)
        assert_eq!(backoff_seconds(0), 5); // edge case: 2^0 * 5 = 5
        assert_eq!(backoff_seconds(1), 5); // first failure: 2^0 * 5 = 5
        assert_eq!(backoff_seconds(2), 10); // second: 2^1 * 5 = 10
        assert_eq!(backoff_seconds(3), 20); // third: 2^2 * 5 = 20
        assert_eq!(backoff_seconds(4), 40);
        assert_eq!(backoff_seconds(5), 80);
        assert_eq!(backoff_seconds(6), 160);
        assert_eq!(backoff_seconds(7), 300); // capped
        // Capped at 300
        assert_eq!(backoff_seconds(8), 300);
        assert_eq!(backoff_seconds(100), 300);
    }

    /// Regression: fail_job with retry did not set retry_after, causing immediate re-execution.
    #[test]
    fn test_fail_job_retry_sets_retry_after() {
        let (_dir, conn) = setup_db();
        let job = insert_job(&conn, "test", "{}", "manual", 3, "default").unwrap();
        conn.execute(
            "UPDATE _crap_jobs SET status = 'running', attempt = 1 WHERE id = ?1",
            &[DbValue::Text(job.id.clone())],
        )
        .unwrap();

        fail_job(&conn, &job.id, "transient error", true, 1).unwrap();
        let fetched = get_job_run(&conn, &job.id).unwrap().unwrap();
        assert_eq!(fetched.status, JobStatus::Pending);
        assert!(
            fetched.retry_after.is_some(),
            "retry_after should be set for backoff"
        );
    }
}
