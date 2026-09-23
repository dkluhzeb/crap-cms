//! The retention purge's multi-node tick claim.

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};

use crate::db::{DbConnection, query::jobs as job_query};

/// Dedup slug used to claim the retention-purge cron tick via
/// `_crap_cron_fired`. Retention purge is a "pseudo cron" job — it runs on a
/// fixed interval from the scheduler loop rather than a user-defined cron
/// expression, but must still be deduped across instances in multi-node
/// deployments.
const RETENTION_PURGE_SLUG: &str = "__retention_purge";

/// Attempt to claim the retention-purge tick for this instance/window.
///
/// Returns `true` iff this caller won the tick and should run the purge.
/// Uses the same `_crap_cron_fired` dedup table as user cron jobs.
/// `window_seconds` must match the scheduler's purge cadence so two instances
/// firing inside the same window still end up with exactly one winner.
pub(in crate::scheduler) fn claim_retention_purge_tick(
    conn: &dyn DbConnection,
    now: DateTime<Utc>,
    window_seconds: i64,
) -> Result<bool> {
    let fired_at = now.to_rfc3339();
    let window_start = (now - Duration::seconds(window_seconds)).to_rfc3339();

    job_query::try_claim_cron_window(conn, RETENTION_PURGE_SLUG, &fired_at, &window_start)
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::scheduler::runner::test_support::make_test_pool;

    // ── claim_retention_purge_tick ────────────────────────────────────────

    /// Two concurrent claims in the same window: only one wins. Locks in the
    /// retention-purge dedup so multi-node deployments don't run the purge N
    /// times per tick.
    #[test]
    fn retention_purge_claims_cron_tick_atomically() {
        let pool = make_test_pool();
        let conn = pool.get().unwrap();

        let now = Utc::now();
        let window_secs = 600; // 10 cron ticks of 60s

        // First call wins.
        let first =
            claim_retention_purge_tick(&conn, now, window_secs).expect("first claim must succeed");
        assert!(first, "first claim in a fresh window should win");

        // Second call immediately after, same window: must lose.
        let second = claim_retention_purge_tick(&conn, now, window_secs)
            .expect("second claim must succeed (returns Ok)");
        assert!(
            !second,
            "second claim inside the same window must return false"
        );

        // A call well past the window: must win again (next tick).
        let later = now + Duration::seconds(window_secs * 2);
        let third = claim_retention_purge_tick(&conn, later, window_secs)
            .expect("later claim must succeed");
        assert!(third, "a claim past the window should win again");
    }
}
