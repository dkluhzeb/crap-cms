//! Job-failure recording shared by every job kind.

use anyhow::{Context as _, Error, Result};
use tracing::{error, warn};

use crate::{
    core::{JobRun, validate::humanize_hook_message},
    db::{DbPool, query::jobs as job_query},
};

/// Borrowed inputs for [`write_job_failure`], grouped per the >4-params rule
/// (mirrors [`ExecuteJobParams`](crate::scheduler::ExecuteJobParams):
/// all-reference fields, `Copy`, literal-built by the two `record_*` wrappers).
///
/// `label` is the human job identifier for log lines (e.g. `"Job abc (slug)"`);
/// `error_msg` is the already-rendered failure text stored on the row.
#[derive(Clone, Copy)]
struct JobFailureWrite<'a> {
    pool: &'a DbPool,
    job_run: &'a JobRun,
    label: &'a str,
    error_msg: &'a str,
    should_retry: bool,
}

/// Write a job-failure outcome to the queue row and log it at the right level.
/// The single place the failure write + retry/permanent log-level split lives,
/// so every job kind records failures identically.
fn write_job_failure(w: JobFailureWrite<'_>) -> Result<()> {
    // The write pool, like every job-row write: a write on a read connection
    // starves the readers the pool split protects.
    let c = w
        .pool
        .write()
        .context("Failed to get DB connection to record job failure")?;

    // The stored error is readable through `GetJobRun`. A hook that raised
    // `crap.validation_error` leaves an encoded marker in the raw chain, and
    // the marker's per-process nonce is what stops forgery — so render it as
    // plain text before it reaches the row or the log.
    let error_msg = humanize_hook_message(w.error_msg);

    job_query::fail_job(
        &c,
        &w.job_run.id,
        &error_msg,
        w.should_retry,
        w.job_run.attempt,
    )?;

    if w.should_retry {
        warn!(
            "{} failed (attempt {}/{}), will retry: {}",
            w.label, w.job_run.attempt, w.job_run.max_attempts, error_msg
        );
    } else {
        error!("{} failed permanently: {}", w.label, error_msg);
    }

    Ok(())
}

/// Record a retryable job failure: render the error with its full anyhow cause
/// chain (`{:#}` — in ONE place, so no job kind silently drops diagnostic detail
/// the way the user-job path used to with `to_string()`) and honor the attempt
/// budget. Shared by the Lua-handler, system-email, and image-convert paths.
pub(super) fn record_job_failure(
    pool: &DbPool,
    job_run: &JobRun,
    label: &str,
    err: &Error,
) -> Result<()> {
    write_job_failure(JobFailureWrite {
        pool,
        job_run,
        label,
        error_msg: &format!("{err:#}"),
        should_retry: job_run.attempt < job_run.max_attempts,
    })
}

/// Record a permanent (never-retried) job failure — e.g. a malformed system-job
/// payload that no retry can fix.
pub(in crate::scheduler) fn record_permanent_job_failure(
    pool: &DbPool,
    job_run: &JobRun,
    label: &str,
    error_msg: &str,
) -> Result<()> {
    write_job_failure(JobFailureWrite {
        pool,
        job_run,
        label,
        error_msg,
        should_retry: false,
    })
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use anyhow::anyhow;

    use super::*;
    use crate::{db::DbConnection, scheduler::runner::test_support::make_test_pool};

    /// Regression: a failed job records the FULL anyhow cause chain (`{:#}`),
    /// not just the top-level message. The user-job path used to `to_string()`
    /// and silently drop the causes that the system-job paths kept; all job
    /// kinds now share `record_job_failure`, so the stored error is uniform.
    #[test]
    fn record_job_failure_stores_full_cause_chain() {
        let pool = make_test_pool();
        let conn = pool.get().unwrap();
        job_query::insert_job(&conn, "my_job", "{}", "manual", 3, "default", 0).unwrap();
        conn.execute_batch("UPDATE _crap_jobs SET status = 'running', attempt = 1")
            .unwrap();

        let job_run = job_query::list_job_runs(&conn, Some("my_job"), None, 1, 0)
            .unwrap()
            .pop()
            .expect("inserted job");
        drop(conn);

        let err = anyhow!("disk write failed").context("could not persist result");
        record_job_failure(&pool, &job_run, "Job (my_job)", &err).unwrap();

        let conn = pool.get().unwrap();
        let stored = job_query::list_job_runs(&conn, Some("my_job"), None, 1, 0)
            .unwrap()
            .pop()
            .and_then(|r| r.error)
            .expect("failure recorded an error");

        assert!(
            stored.contains("could not persist result"),
            "outer message missing: {stored}"
        );
        assert!(
            stored.contains("disk write failed"),
            "root cause dropped (not rendered with {{:#}}): {stored}"
        );
    }
}
