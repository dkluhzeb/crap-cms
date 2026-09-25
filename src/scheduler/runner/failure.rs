//! Job-failure recording shared by every job kind.

use anyhow::{Context as _, Error, Result};
use chrono::{DateTime, Utc};
use tracing::{error, info, warn};

use crate::{
    core::{JobRun, validate::humanize_hook_message},
    db::{
        DbPool,
        query::{jobs as job_query, jobs::Deferral},
    },
};

/// How long after it was queued a job may still be deferred. Past this a
/// deferrable condition counts as an ordinary failure and spends an attempt,
/// so a job that never gets its resource still ends.
const MAX_DEFER_AGE_SECS: u64 = 6 * 60 * 60;

/// The shortest and longest wait before a deferred job runs again.
const MIN_DEFER_DELAY_SECS: u64 = 15;
const MAX_DEFER_DELAY_SECS: u64 = 300;

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

/// Seconds since the job was queued; `0` when the stored time does not parse.
fn job_age_secs(job_run: &JobRun, now: DateTime<Utc>) -> u64 {
    let Some(created) = job_run.created_at.as_deref() else {
        return 0;
    };

    let parsed = DateTime::parse_from_rfc3339(created)
        .or_else(|_| DateTime::parse_from_rfc3339(&created.replacen(' ', "T", 1)));
    let Ok(created) = parsed else {
        return 0;
    };

    u64::try_from((now - created.with_timezone(&Utc)).num_seconds()).unwrap_or(0)
}

/// The wait before a deferred job runs again: a tenth of its age, clamped —
/// so a job deferred again and again backs off as it ages.
fn defer_delay_secs(age_secs: u64) -> u64 {
    (age_secs / 10).clamp(MIN_DEFER_DELAY_SECS, MAX_DEFER_DELAY_SECS)
}

/// Record a run that found a shared resource busy (e.g. every image-processing
/// slot taken): the job goes back to the queue with a backoff *without
/// consuming an attempt*, since nothing about the job itself failed.
///
/// Bounded: a job queued more than [`MAX_DEFER_AGE_SECS`] ago is recorded as an
/// ordinary failure instead ([`record_job_failure`]), so the attempt budget
/// still ends a job whose resource never frees up.
pub(super) fn record_job_deferral(
    pool: &DbPool,
    job_run: &JobRun,
    label: &str,
    err: &Error,
) -> Result<()> {
    let delay = defer_delay_secs(job_age_secs(job_run, Utc::now()));
    let reason = format!("{err:#}");

    let c = pool
        .write()
        .context("Failed to get DB connection to defer job")?;

    let deferral = Deferral::new(delay, MAX_DEFER_AGE_SECS);

    if job_query::defer_job(&c, job_run, &reason, deferral)? {
        info!("{label} deferred ({reason}); runs again in {delay}s without using an attempt");

        return Ok(());
    }

    drop(c);

    record_job_failure(pool, job_run, label, err)
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use anyhow::anyhow;

    use super::*;
    use crate::{
        core::{JobStatus, ScheduledBy},
        db::{DbConnection, DbValue},
        scheduler::runner::test_support::make_test_pool,
    };

    /// A job claimed once (running at attempt 1 of 3), queued `age_secs` ago.
    fn claimed_job(pool: &DbPool, age_secs: u64) -> JobRun {
        let conn = pool.get().unwrap();
        let job = job_query::insert_job(&conn, "convert", "{}", ScheduledBy::Cli, 3, "default", 0)
            .unwrap();

        conn.execute(
            "UPDATE _crap_jobs SET status = 'running', attempt = 1, \
             created_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?2) WHERE id = ?1",
            &[
                DbValue::Text(job.id.clone()),
                DbValue::Text(format!("-{age_secs} seconds")),
            ],
        )
        .unwrap();

        job_query::get_job_run(&conn, &job.id).unwrap().unwrap()
    }

    fn stored(pool: &DbPool, id: &str) -> JobRun {
        job_query::get_job_run(&pool.get().unwrap(), id)
            .unwrap()
            .unwrap()
    }

    /// Regression: an image conversion that found every processing slot taken
    /// spent an attempt like a real failure, so a burst of uploads could fail
    /// conversions for good that nothing was wrong with.
    #[test]
    fn a_deferred_run_keeps_its_attempt() {
        let pool = make_test_pool();
        let job = claimed_job(&pool, 5);

        record_job_deferral(&pool, &job, "Job", &anyhow!("busy")).unwrap();

        let row = stored(&pool, &job.id);
        assert_eq!(row.status, JobStatus::Pending);
        assert_eq!(row.attempt, 0, "a deferral must not consume an attempt");
    }

    /// The bound: past the maximum age a deferrable run is an ordinary failure
    /// and spends its attempt, so a job cannot wait forever.
    #[test]
    fn a_run_past_the_defer_window_spends_its_attempt() {
        let pool = make_test_pool();
        let job = claimed_job(&pool, MAX_DEFER_AGE_SECS + 60);

        record_job_deferral(&pool, &job, "Job", &anyhow!("busy")).unwrap();

        let row = stored(&pool, &job.id);
        assert_eq!(row.status, JobStatus::Pending, "attempt 1 of 3 retries");
        assert_eq!(row.attempt, 1, "the attempt is spent");
    }

    #[test]
    fn the_defer_delay_grows_with_age_within_bounds() {
        assert_eq!(defer_delay_secs(0), MIN_DEFER_DELAY_SECS);
        assert_eq!(defer_delay_secs(600), 60);
        assert_eq!(defer_delay_secs(MAX_DEFER_AGE_SECS), MAX_DEFER_DELAY_SECS);
    }

    #[test]
    fn the_job_age_reads_both_stored_timestamp_shapes() {
        let now = DateTime::parse_from_rfc3339("2026-01-01T00:10:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut job = JobRun::builder("j", "s").build();

        job.created_at = Some("2026-01-01T00:00:00.000Z".into());
        assert_eq!(job_age_secs(&job, now), 600);

        job.created_at = Some("2026-01-01 00:00:00+00:00".into());
        assert_eq!(job_age_secs(&job, now), 600);

        job.created_at = Some("garbage".into());
        assert_eq!(job_age_secs(&job, now), 0);
    }

    /// Regression: a failed job records the FULL anyhow cause chain (`{:#}`),
    /// not just the top-level message. The user-job path used to `to_string()`
    /// and silently drop the causes that the system-job paths kept; all job
    /// kinds now share `record_job_failure`, so the stored error is uniform.
    #[test]
    fn record_job_failure_stores_full_cause_chain() {
        let pool = make_test_pool();
        let conn = pool.get().unwrap();
        job_query::insert_job(&conn, "my_job", "{}", ScheduledBy::Cli, 3, "default", 0).unwrap();
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
