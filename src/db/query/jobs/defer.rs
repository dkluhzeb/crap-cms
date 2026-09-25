//! Deferring a running job: back to `pending` without consuming an attempt.

use anyhow::{Context as _, Result};

use crate::{
    core::JobRun,
    db::{DbConnection, DbValue},
};

/// When a deferred job runs again, and how long after it was queued deferring
/// stays allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Deferral {
    /// Seconds from now until the job is claimable again.
    pub delay_secs: u64,
    /// The job's maximum age (since it was queued) at which it may still be
    /// deferred; an older job is left running for the caller to fail.
    pub max_age_secs: u64,
}

impl Deferral {
    #[must_use]
    pub fn new(delay_secs: u64, max_age_secs: u64) -> Self {
        Self {
            delay_secs,
            max_age_secs,
        }
    }
}

/// Put a running job back to `pending` for a later run *without consuming an
/// attempt*: the claim's increment is undone, so the retry budget is spent only
/// on real failures. `reason` is stored as the row's error so the wait is
/// visible.
///
/// Returns `false` — writing nothing — when the job was queued more than
/// `deferral.max_age_secs` ago (the caller then records an ordinary failure, so
/// a job cannot be deferred forever), or when the row is no longer this
/// attempt's running row (the same compare-and-set as `fail_job`).
///
/// # Errors
///
/// Returns a backend error if the UPDATE fails.
pub fn defer_job(
    conn: &dyn DbConnection,
    job_run: &JobRun,
    reason: &str,
    deferral: Deferral,
) -> Result<bool> {
    let delay = i64::try_from(deferral.delay_secs).context("defer delay out of range")?;
    let max_age = i64::try_from(deferral.max_age_secs).context("defer max age out of range")?;

    // A negative offset is a future time under `date_offset_expr`'s
    // `now - seconds` contract.
    let (retry_sql, retry_param) = conn.date_offset_expr(-delay, 3);
    let (oldest_sql, oldest_param) = conn.date_offset_expr(max_age, 5);
    let (p1, p2, p4) = (
        conn.placeholder(1),
        conn.placeholder(2),
        conn.placeholder(4),
    );

    let updated = conn
        .execute(
            &format!(
                "UPDATE _crap_jobs SET status = 'pending', attempt = attempt - 1, error = {p2}, \
                 started_at = NULL, heartbeat_at = NULL, retry_after = {retry_sql} \
                 WHERE id = {p1} AND status = 'running' AND attempt = {p4} \
                   AND created_at >= {oldest_sql}"
            ),
            &[
                DbValue::Text(job_run.id.clone()),
                DbValue::Text(reason.to_string()),
                retry_param,
                DbValue::Integer(i64::from(job_run.attempt)),
                oldest_param,
            ],
        )
        .context("Failed to defer job")?;

    Ok(updated > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        core::{JobStatus, ScheduledBy},
        db::query::jobs::{get_job_run, insert_job, test_helpers::setup_db},
    };

    /// A job claimed once (running at attempt 1), queued `age_secs` ago.
    fn running_job(conn: &dyn DbConnection, age_secs: u64) -> JobRun {
        let job = insert_job(conn, "convert", "{}", ScheduledBy::Cli, 3, "default", 0).unwrap();

        conn.execute(
            "UPDATE _crap_jobs SET status = 'running', attempt = 1, \
             created_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?2) WHERE id = ?1",
            &[
                DbValue::Text(job.id.clone()),
                DbValue::Text(format!("-{age_secs} seconds")),
            ],
        )
        .unwrap();

        get_job_run(conn, &job.id).unwrap().unwrap()
    }

    #[test]
    fn a_deferred_job_is_pending_again_with_its_attempt_given_back() {
        let (_dir, conn) = setup_db();
        let job = running_job(&conn, 10);

        let deferred = defer_job(&conn, &job, "busy", Deferral::new(60, 3600)).unwrap();

        assert!(deferred);

        let row = get_job_run(&conn, &job.id).unwrap().unwrap();
        assert_eq!(row.status, JobStatus::Pending);
        assert_eq!(row.attempt, 0, "the claim's attempt must be given back");
        assert_eq!(row.error.as_deref(), Some("busy"));
        assert!(row.started_at.is_none());
        assert!(
            row.retry_after.is_some(),
            "the job waits before it runs again"
        );
    }

    #[test]
    fn a_deferred_job_is_not_claimable_before_its_delay() {
        let (_dir, conn) = setup_db();
        let job = running_job(&conn, 10);

        defer_job(&conn, &job, "busy", Deferral::new(60, 3600)).unwrap();

        let due = conn
            .query_one(
                "SELECT COUNT(*) FROM _crap_jobs \
                 WHERE retry_after <= strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
                &[],
            )
            .unwrap()
            .and_then(|r| r.i64_at(0));
        assert_eq!(due, Some(0));
    }

    /// The bound that stops a job from being deferred forever: past its
    /// maximum age the row is left alone for the caller's ordinary failure.
    #[test]
    fn a_job_past_the_maximum_age_is_not_deferred() {
        let (_dir, conn) = setup_db();
        let job = running_job(&conn, 7200);

        let deferred = defer_job(&conn, &job, "busy", Deferral::new(60, 3600)).unwrap();

        assert!(!deferred);

        let row = get_job_run(&conn, &job.id).unwrap().unwrap();
        assert_eq!(row.status, JobStatus::Running);
        assert_eq!(row.attempt, 1);
    }

    /// Compare-and-set like every job-row write: a write from an attempt the
    /// row has moved past changes nothing.
    #[test]
    fn a_stale_attempt_cannot_defer_the_row() {
        let (_dir, conn) = setup_db();
        let mut job = running_job(&conn, 10);
        job.attempt = 2;

        let deferred = defer_job(&conn, &job, "busy", Deferral::new(60, 3600)).unwrap();

        assert!(!deferred);
        assert_eq!(
            get_job_run(&conn, &job.id).unwrap().unwrap().attempt,
            1,
            "the live row must be untouched"
        );
    }
}
