//! The heartbeat tick: refresh this node's running-job heartbeats, then
//! reclaim any dead peer's jobs — and the stale threshold both halves share.

use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result};
use tracing::{error, warn};

use crate::{
    core::Registry,
    db::{DbPool, query::jobs as job_query},
    service,
};

use super::{runner::recover_stale_jobs, types::DbTimeouts};

/// Seconds without a heartbeat after which a `running` job counts as dead,
/// with this module's timeout bundle unpacked for the shared rule.
///
/// The arithmetic lives in [`service::jobs::stale_threshold_secs`] because
/// the health check reports against the same line the reclaim acts on.
pub(super) fn stale_threshold_secs(heartbeat_interval: u64, timeouts: &DbTimeouts) -> u64 {
    service::jobs::stale_threshold_secs(
        heartbeat_interval,
        timeouts.connection_timeout_secs,
        timeouts.busy_timeout_ms,
    )
}

/// What one heartbeat tick needs; owned so it can move to the blocking pool.
pub(super) struct HeartbeatTickInput {
    pub pool: DbPool,
    pub registry: Arc<Registry>,
    pub running_jobs: Arc<Mutex<Vec<String>>>,
    pub stale_threshold_secs: u64,
}

/// Refresh this node's own running-job heartbeats first, then reclaim any
/// DEAD peer's jobs (heartbeat expired past the threshold) — the runtime half
/// of the at-least-once recovery, so a crashed worker's jobs don't wait for
/// that node to restart.
#[cfg(not(tarpaulin_include))]
pub(super) fn heartbeat_tick(t: &HeartbeatTickInput) {
    update_heartbeats(&t.pool, &t.running_jobs);

    let conn = match t.pool.write() {
        Ok(conn) => conn,
        Err(e) => {
            error!("Scheduler stale-recovery error: no write connection: {e}");

            return;
        }
    };

    if let Err(e) = recover_stale_jobs(&conn, &t.registry, t.stale_threshold_secs) {
        error!("Scheduler stale-recovery error: {}", e);
    }
}

/// Update heartbeats for all currently running jobs.
#[cfg(not(tarpaulin_include))]
fn update_heartbeats(pool: &DbPool, running_jobs: &Arc<Mutex<Vec<String>>>) {
    let ids: Vec<String> = running_jobs
        .lock()
        .map(|guard| guard.clone())
        .unwrap_or_default();

    if ids.is_empty() {
        return;
    }

    // The write pool: each heartbeat is an autocommit UPDATE, and a write on a
    // read connection starves the readers the pool split protects.
    let conn = match pool.write() {
        Ok(conn) => conn,
        Err(e) => {
            warn!("Heartbeat update skipped: no write connection: {e}");

            return;
        }
    };

    for id in &ids {
        if let Err(e) = job_query::update_heartbeat(&conn, id) {
            warn!("Heartbeat update error for {}: {}", id, e);
        }
    }
}

/// Recover stale jobs on startup. (Image queue recovery is handled by
/// `recover_stale_jobs` too, because image conversion lives in the unified
/// job queue as `_system_image_convert` jobs.)
///
/// # Errors
///
/// Propagates a connection or stale-job recovery failure.
#[cfg(not(tarpaulin_include))]
pub(super) fn recover_on_startup(
    pool: &DbPool,
    registry: &Registry,
    stale_threshold_secs: u64,
) -> Result<()> {
    let conn = pool
        .write()
        .context("Scheduler: failed to get DB connection for recovery")?;

    recover_stale_jobs(&conn, registry, stale_threshold_secs)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The framework defaults: a 10s heartbeat, a 30s busy timeout and a 10s
    /// connection timeout give 30 + 10 + 30 = 70s.
    #[test]
    fn the_threshold_adds_the_write_delays_to_three_intervals() {
        assert_eq!(stale_threshold_secs(10, &DbTimeouts::new(30_000, 10)), 70);
    }

    /// Regression: the threshold was `heartbeat_interval * 3` alone, so a
    /// heartbeat blocked for the whole busy timeout crossed it and the still
    /// running job was requeued. The threshold must exceed the worst-case
    /// delay of one heartbeat write by at least two intervals of slack.
    #[test]
    fn the_threshold_exceeds_the_worst_case_heartbeat_write_delay() {
        let interval = 10;
        let timeouts = DbTimeouts::new(30_000, 10);
        let worst_case_write_delay = timeouts.connection_timeout_secs + 30;

        let threshold = stale_threshold_secs(interval, &timeouts);

        assert!(
            threshold >= worst_case_write_delay + 2 * interval,
            "{threshold}s leaves no slack over a {worst_case_write_delay}s write"
        );
    }

    #[test]
    fn a_partial_second_of_busy_timeout_rounds_up() {
        assert_eq!(stale_threshold_secs(10, &DbTimeouts::new(1_500, 0)), 32);
    }

    #[test]
    fn zero_timeouts_leave_the_three_interval_slack() {
        assert_eq!(stale_threshold_secs(10, &DbTimeouts::new(0, 0)), 30);
    }

    #[test]
    fn the_threshold_saturates_instead_of_overflowing() {
        assert_eq!(
            stale_threshold_secs(u64::MAX, &DbTimeouts::new(u64::MAX, u64::MAX)),
            u64::MAX
        );
    }
}
