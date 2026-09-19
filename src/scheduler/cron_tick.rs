//! The cron tick: evaluate the cron schedules for the window since the last
//! successful tick, and every tenth tick run the retention purges.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
};

use anyhow::{Context as _, Result};
use chrono::{DateTime, Utc};
use tracing::{debug, error, info, warn};

use crate::{
    core::upload::delete_storage_keys,
    db::{BoxedConnection, query::jobs as job_query},
    service::AppInfra,
};

use super::runner::{check_cron_schedules, claim_retention_purge_tick, purge_soft_deleted};

/// The purge runs every this many cron ticks.
const PURGE_EVERY_TICKS: u64 = 10;

/// The retention purges' cadence and their configured job-row retention.
pub(super) struct PurgeSchedule {
    /// Cron ticks seen so far; the purge runs on every tenth.
    pub counter: Arc<AtomicU64>,
    /// `[jobs] auto_purge`: finished job rows older than this are purged.
    pub auto_purge_secs: Option<u64>,
    pub cron_interval_secs: i64,
}

/// What one cron tick needs; owned so it can move to the blocking pool.
pub(super) struct CronTickInput {
    pub infra: Arc<AppInfra>,
    pub queue_retries: Arc<HashMap<String, u32>>,
    /// The end of the last window a cron tick SUCCEEDED for.
    pub last_cron_check: Arc<Mutex<DateTime<Utc>>>,
    pub purge: PurgeSchedule,
}

/// One cron tick: enqueue the cron jobs due in `(last_check, now]`, then
/// count the tick towards the periodic purges.
#[cfg(not(tarpaulin_include))]
pub(super) fn cron_tick(t: &CronTickInput) {
    let now = Utc::now();
    let last_check = *t
        .last_cron_check
        .lock()
        .unwrap_or_else(PoisonError::into_inner);

    // Only advance `last_cron_check` when the tick SUCCEEDS. The whole tick
    // runs in one transaction; a transient failure rolls back every slug's
    // enqueue for the window `(last_check, now]`, so advancing
    // unconditionally would silently drop all cron jobs due in that window —
    // the next tick must re-cover it.
    match check_cron_schedules(
        &t.infra.pool,
        &t.infra.registry,
        last_check,
        now,
        &t.queue_retries,
    ) {
        Ok(()) => {
            *t.last_cron_check
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = now;
        }
        Err(e) => error!("Scheduler cron error (window will be retried): {}", e),
    }

    let ticks = t.purge.counter.fetch_add(1, Ordering::SeqCst) + 1;

    if ticks.is_multiple_of(PURGE_EVERY_TICKS) {
        run_periodic_purges(t);
    }
}

/// Run the periodic purges (every tenth cron tick).
///
/// In multi-node deployments the purge is gated by an atomic
/// `_crap_cron_fired` claim -- only one node runs it per purge window. The
/// claim and the purges it stands for are ONE transaction: a claim recorded
/// before the purge ran would, on a crash in between, skip that window's
/// purge entirely — no node re-claims a window already marked fired. Either
/// the window is claimed with its purge durable, or neither happened and the
/// next window (this node's or a peer's) runs it.
///
/// Upload files are deleted only AFTER that transaction commits: a rollback
/// leaves orphaned files (safe), never DB rows pointing at deleted files.
#[cfg(not(tarpaulin_include))]
fn run_periodic_purges(t: &CronTickInput) {
    let mut conn = match t.infra.pool.write() {
        Ok(conn) => conn,
        Err(e) => {
            warn!("Retention purge skipped: no write connection: {e}");

            return;
        }
    };

    let keys_to_clean = match claim_and_purge(&mut conn, t) {
        Ok(Some(keys)) => keys,
        Ok(None) => {
            debug!("Retention purge already claimed by another instance this window");

            return;
        }
        Err(e) => {
            warn!("Retention purge error (rolled back, window unclaimed): {e:#}");

            return;
        }
    };

    delete_storage_keys(&*t.infra.storage, &keys_to_clean);
}

/// Claim this purge window and run both purges inside the claim's IMMEDIATE
/// transaction. `Ok(None)` = another instance holds the window. Returns the
/// upload field-maps whose files the caller deletes once this has committed.
///
/// `transaction_immediate()`: the claim runs a SELECT and an INSERT/UPDATE on
/// `_crap_cron_fired`, and the soft-delete purge's per-doc locked ref-count
/// check + delete relies on the same write lock — a deferred transaction
/// would hit `SQLITE_BUSY_SNAPSHOT` when a concurrent writer commits between
/// the read and the write.
#[cfg(not(tarpaulin_include))]
fn claim_and_purge(conn: &mut BoxedConnection, t: &CronTickInput) -> Result<Option<Vec<String>>> {
    // The purge fires every tenth cron tick, so the dedup window must cover
    // that span -- otherwise two nodes drifting by ~1 cron tick would each
    // claim a fresh window and run the purge twice.
    let window_secs = t
        .purge
        .cron_interval_secs
        .saturating_mul(i64::try_from(PURGE_EVERY_TICKS).unwrap_or(i64::MAX));

    let tx = conn
        .transaction_immediate()
        .context("open the retention-purge transaction")?;

    if !claim_retention_purge_tick(&tx, Utc::now(), window_secs)? {
        return Ok(None);
    }

    // Job-row retention: gated behind the same single-winner claim as the
    // soft-delete purge, so only one node does the work per window.
    if let Some(secs) = t.purge.auto_purge_secs {
        let purged = job_query::purge_old_jobs(&tx, secs).context("purge old job runs")?;

        if purged > 0 {
            info!("Auto-purged {} old job run(s)", purged);
        }
    }

    let (purged, files) = purge_soft_deleted(&tx, &t.infra.registry, &t.infra.locale_config)
        .context("purge expired soft-deleted documents")?;

    tx.commit().context("commit the retention purge")?;

    if purged > 0 {
        info!("Purged {purged} expired soft-deleted doc(s)");
    }

    Ok(Some(files))
}
