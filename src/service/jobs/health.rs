//! Job-system health — the probes and the verdict, in one place.
//!
//! `crap-cms jobs healthcheck` owned this: it gathered the probes, classified
//! them, and printed them in one function, with its own idea of when a running
//! job is stale. That idea had drifted from the scheduler's, which is the one
//! that actually reclaims work, so the check reported a dead worker for jobs
//! the scheduler considered perfectly alive. [`stale_threshold_secs`] is now
//! the single rule and the scheduler's heartbeat delegates to it.

use anyhow::Result;

use crate::{
    config::CrapConfig,
    core::{Registry, job::JobRun},
    db::{DbConnection, query::jobs as job_query},
};

/// Seconds a `running` job may go without a heartbeat before it counts as
/// dead: three heartbeat intervals of slack (two missed beats), plus the
/// longest a single heartbeat write can legitimately take on a healthy node —
/// waiting for a write-pool connection, then for the database's write lock.
///
/// Without that allowance a heartbeat merely held up behind a long write (a
/// bulk batch, a `crap.transaction` block) crosses the bare three-interval
/// mark, and stale recovery requeues a job that is still running: a second
/// execution of the same run.
///
/// Both the scheduler's reclaim and the health check read this, so the system
/// cannot report a job dead that it would not also reclaim.
#[must_use]
pub fn stale_threshold_secs(
    heartbeat_interval: u64,
    connection_timeout_secs: u64,
    busy_timeout_ms: u64,
) -> u64 {
    heartbeat_interval
        .saturating_mul(STALE_HEARTBEAT_MULTIPLIER)
        .saturating_add(connection_timeout_secs)
        .saturating_add(busy_timeout_ms.div_ceil(1000))
}

/// Heartbeat intervals of slack before a running job is presumed dead. Must
/// be > 1 so a single missed tick doesn't reclaim a live job.
const STALE_HEARTBEAT_MULTIPLIER: u64 = 3;

/// Seconds of failures the check looks back over.
const FAILURE_WINDOW_SECS: u64 = 86_400;

/// How long a run may sit pending before it counts as backed up.
const PENDING_WARN_SECS: u64 = 300;

/// The verdict. Ordered by severity so the worst probe wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobHealthStatus {
    /// Nothing to report.
    Healthy,
    /// Work is failing or backing up, but the workers are alive.
    Warning,
    /// A worker stopped heartbeating mid-run.
    Unhealthy,
}

impl JobHealthStatus {
    /// Lower-case wire name, shared by the CLI output and the Lua table.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Warning => "warning",
            Self::Unhealthy => "unhealthy",
        }
    }
}

/// What the probes found, plus the verdict they add up to.
#[derive(Debug, Clone)]
pub struct JobHealthReport {
    /// Jobs defined in the registry.
    pub defined: usize,
    /// Running jobs whose worker stopped heartbeating, so a caller can name
    /// them rather than only count them.
    pub stale: Vec<JobRun>,
    /// Runs that failed within the last day.
    pub failed_recently: i64,
    /// Runs pending longer than the backed-up threshold.
    pub pending_long: i64,
    /// Scheduled jobs that have never completed a run, in slug order.
    pub never_ran: Vec<String>,
    /// The verdict.
    pub status: JobHealthStatus,
}

/// Classify the probes: a worker that stopped heartbeating mid-run is the
/// only unhealthy signal, because it means work is stuck rather than merely
/// going badly. Failures, a backed-up queue, and a scheduled job that has
/// never completed are warnings.
fn classify(
    stale: usize,
    failed_recently: i64,
    pending_long: i64,
    never_ran: usize,
) -> JobHealthStatus {
    if stale > 0 {
        return JobHealthStatus::Unhealthy;
    }

    if failed_recently > 0 || pending_long > 0 || never_ran > 0 {
        return JobHealthStatus::Warning;
    }

    JobHealthStatus::Healthy
}

/// Scheduled jobs that have never completed a run, in slug order.
fn never_completed(conn: &dyn DbConnection, registry: &Registry) -> Result<Vec<String>> {
    let mut slugs = Vec::new();

    for (slug, def) in &registry.jobs {
        if def.schedule.is_some() && job_query::last_completed_run(conn, slug)?.is_none() {
            slugs.push(slug.to_string());
        }
    }

    slugs.sort();

    Ok(slugs)
}

/// Probe the job system and classify what comes back.
///
/// # Errors
///
/// Returns an error when a probe query fails.
pub fn check_job_health(
    conn: &dyn DbConnection,
    registry: &Registry,
    config: &CrapConfig,
) -> Result<JobHealthReport> {
    let threshold = stale_threshold_secs(
        config.jobs.heartbeat_interval,
        config.database.connection_timeout,
        config.database.busy_timeout,
    );

    let stale = job_query::find_stale_jobs(conn, threshold)?;
    let failed_recently = job_query::count_failed_since(conn, FAILURE_WINDOW_SECS)?;
    let pending_long = job_query::count_pending_older_than(conn, PENDING_WARN_SECS)?;
    let never_ran = never_completed(conn, registry)?;

    Ok(JobHealthReport {
        defined: registry.jobs.len(),
        status: classify(stale.len(), failed_recently, pending_long, never_ran.len()),
        stale,
        failed_recently,
        pending_long,
        never_ran,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stale worker outranks everything: work is stuck, not just failing.
    #[test]
    fn a_stale_worker_is_unhealthy_whatever_else_is_true() {
        assert_eq!(classify(1, 0, 0, 0), JobHealthStatus::Unhealthy);
        assert_eq!(classify(1, 9, 9, 9), JobHealthStatus::Unhealthy);
    }

    /// Each of the softer probes on its own is a warning.
    #[test]
    fn failures_backlog_and_never_ran_are_warnings() {
        assert_eq!(classify(0, 1, 0, 0), JobHealthStatus::Warning);
        assert_eq!(classify(0, 0, 1, 0), JobHealthStatus::Warning);
        assert_eq!(classify(0, 0, 0, 1), JobHealthStatus::Warning);
    }

    #[test]
    fn nothing_to_report_is_healthy() {
        assert_eq!(classify(0, 0, 0, 0), JobHealthStatus::Healthy);
    }

    /// The documented arithmetic: three intervals plus the two write delays.
    #[test]
    fn the_threshold_adds_the_write_delays_to_three_intervals() {
        assert_eq!(stale_threshold_secs(10, 10, 30_000), 70);
    }

    /// Regression: the threshold must exceed the worst case a single
    /// heartbeat write can take, by at least two intervals of slack —
    /// otherwise a heartbeat stuck behind a long write looks like a dead
    /// worker.
    #[test]
    fn the_threshold_exceeds_the_worst_case_heartbeat_write_delay() {
        let interval = 10;
        let connection_timeout = 10;
        let busy_timeout_ms = 30_000;
        let worst_case_write_delay = connection_timeout + busy_timeout_ms / 1000;

        let threshold = stale_threshold_secs(interval, connection_timeout, busy_timeout_ms);

        assert!(
            threshold >= worst_case_write_delay + 2 * interval,
            "threshold {threshold} leaves no slack over a {worst_case_write_delay}s write"
        );
    }

    /// A sub-second busy timeout still contributes a whole second, so the
    /// allowance is never rounded away.
    #[test]
    fn a_sub_second_busy_timeout_still_counts() {
        assert_eq!(stale_threshold_secs(1, 0, 1), 4);
    }
}
