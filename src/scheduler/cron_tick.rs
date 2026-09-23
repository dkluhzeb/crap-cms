//! The cron tick: evaluate the cron schedules for the window since the last
//! successful tick, and every tenth tick run the retention purges.
//!
//! A `--no-cron` worker skips the schedule evaluation and keeps the purges —
//! see [`cron_mode`] for why the two are separable.

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
    db::{DbConnection, query::jobs as job_query},
    service::AppInfra,
};

use super::runner::{
    PurgeBatch, RetentionPurge, check_cron_schedules, claim_retention_purge_tick,
    purge_soft_deleted,
};

/// The purge runs every this many cron ticks.
const PURGE_EVERY_TICKS: u64 = 10;

/// What a cron tick does on this process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CronMode {
    /// Evaluate the cron schedules and run the retention purges.
    Full,
    /// Run the retention purges only.
    PurgeOnly,
}

/// The cron work a process does, from its `run_cron` setting.
///
/// `--no-cron` takes a worker out of cron *scheduling* — the case it exists
/// for is a fleet where one process owns the schedules and the rest only
/// execute. The retention purges keep running on every process, because they
/// are not cron jobs: they are claim-gated single-winner housekeeping
/// (`claim_retention_purge_tick` hands each window to exactly one node, and
/// the losers pay a single SELECT), and in a deployment of
/// `serve --no-scheduler` app servers beside `work --no-cron` workers there
/// is no other process that would ever run them. Dropping the tick there
/// would silently keep finished job rows and expired trash forever.
pub(super) const fn cron_mode(run_cron: bool) -> CronMode {
    if run_cron {
        CronMode::Full
    } else {
        CronMode::PurgeOnly
    }
}

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
    /// Whether this tick evaluates the cron schedules; see [`cron_mode`].
    pub mode: CronMode,
}

/// Enqueue the cron jobs due in `(last_check, now]`.
///
/// Only advances `last_cron_check` when the evaluation SUCCEEDS. The whole
/// evaluation runs in one transaction; a transient failure rolls back every
/// slug's enqueue for the window `(last_check, now]`, so advancing
/// unconditionally would silently drop all cron jobs due in that window —
/// the next tick must re-cover it.
#[cfg(not(tarpaulin_include))]
fn evaluate_schedules(t: &CronTickInput) {
    let now = Utc::now();
    let last_check = *t
        .last_cron_check
        .lock()
        .unwrap_or_else(PoisonError::into_inner);

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
}

/// One cron tick: evaluate the schedules unless this process leaves them to
/// its peers, then count the tick towards the periodic purges — which run on
/// every process, `--no-cron` included (see [`cron_mode`]).
#[cfg(not(tarpaulin_include))]
pub(super) fn cron_tick(t: &CronTickInput) {
    if t.mode == CronMode::Full {
        evaluate_schedules(t);
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
/// claim commits in ONE transaction with the job-row purge and the first
/// batch of the soft-delete purge: a claim recorded before any purge ran
/// would, on a crash in between, skip that window's purge entirely — no node
/// re-claims a window already marked fired. Either the window is claimed with
/// its first batch durable, or neither happened and the next window (this
/// node's or a peer's) runs it.
///
/// The soft-delete purge then continues in bounded batches, each its own
/// IMMEDIATE transaction, until every expired candidate has been examined —
/// so neither the write lock nor the purged rows held in memory grow with the
/// size of the expired trash. A batch that fails rolls back alone: the
/// batches before it stay committed, and what it would have purged is still
/// expired for the next window. A batch that failed on one document is re-run
/// without it (see [`RetentionPurge`]).
///
/// Each batch's upload files are deleted only AFTER it commits: a rollback
/// leaves orphaned files (safe), never DB rows pointing at deleted files. Its
/// purged documents' delete events are published after it too, as every
/// write's are — a rolled-back batch announces nothing.
#[cfg(not(tarpaulin_include))]
fn run_periodic_purges(t: &CronTickInput) {
    let mut run = RetentionPurge::new(t.infra.event_transport.is_some());
    let mut claimed = false;

    loop {
        let batch = match purge_batch(t, &mut run, claimed) {
            Ok(Some(batch)) => batch,
            Ok(None) => {
                debug!("Retention purge already claimed by another instance this window");

                return;
            }
            Err(e) => {
                if run.retry_after_failure() {
                    warn!(
                        "Retention purge batch rolled back; re-running it without the \
                         failed document: {e:#}"
                    );

                    continue;
                }

                warn!("Retention purge error (batch rolled back; the next window retries): {e:#}");

                return;
            }
        };

        finish_batch(t, batch);
        claimed = true;

        if run.is_done() {
            return;
        }
    }
}

/// Delete a committed batch's upload files and publish its delete events.
#[cfg(not(tarpaulin_include))]
fn finish_batch(t: &CronTickInput, batch: PurgeBatch) {
    delete_storage_keys(&*t.infra.storage, &batch.files);

    batch.events.settle(&t.infra);
}

/// Run one batch of the soft-delete purge in its own IMMEDIATE transaction —
/// the first (`claimed` unset) after claiming the purge window and purging
/// old job runs in that same transaction. `Ok(None)` = another instance holds
/// the window. Returns the committed batch, whose files the caller deletes and
/// whose delete events it publishes.
///
/// `transaction_immediate()`: the claim runs a SELECT and an INSERT/UPDATE on
/// `_crap_cron_fired`, and the soft-delete purge's per-doc locked ref-count
/// check + delete relies on the same write lock — a deferred transaction
/// would hit `SQLITE_BUSY_SNAPSHOT` when a concurrent writer commits between
/// the read and the write. The write connection is taken per batch, so other
/// writers get it between batches.
#[cfg(not(tarpaulin_include))]
fn purge_batch(
    t: &CronTickInput,
    run: &mut RetentionPurge,
    claimed: bool,
) -> Result<Option<PurgeBatch>> {
    let mut conn = t.infra.pool.write().context("acquire a write connection")?;

    let tx = conn
        .transaction_immediate()
        .context("open the retention-purge transaction")?;

    if !claimed && !claim_window(&tx, t)? {
        return Ok(None);
    }

    let batch = purge_soft_deleted(&tx, &t.infra.registry, &t.infra.locale_config, run)?;

    tx.commit().context("commit the retention purge")?;

    if batch.purged > 0 {
        info!("Purged {} expired soft-deleted doc(s)", batch.purged);
    }

    Ok(Some(batch))
}

/// Claim this purge window and, behind the same single-winner claim, purge
/// old job runs. `false` = another instance holds the window.
#[cfg(not(tarpaulin_include))]
fn claim_window(tx: &dyn DbConnection, t: &CronTickInput) -> Result<bool> {
    // The purge fires every tenth cron tick, so the dedup window must cover
    // that span -- otherwise two nodes drifting by ~1 cron tick would each
    // claim a fresh window and run the purge twice.
    let window_secs = t
        .purge
        .cron_interval_secs
        .saturating_mul(i64::try_from(PURGE_EVERY_TICKS).unwrap_or(i64::MAX));

    if !claim_retention_purge_tick(tx, Utc::now(), window_secs)? {
        return Ok(false);
    }

    if let Some(secs) = t.purge.auto_purge_secs {
        let purged = job_query::purge_old_jobs(tx, secs).context("purge old job runs")?;

        if purged > 0 {
            info!("Auto-purged {} old job run(s)", purged);
        }
    }

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `crap-cms work --no-cron` takes the worker out of schedule
    /// evaluation — and out of it only: the retention purges are not cron
    /// jobs, and a fleet of `serve --no-scheduler` beside `--no-cron`
    /// workers has no other process that would run them.
    #[test]
    fn no_cron_stops_evaluating_schedules_but_keeps_purging() {
        assert_eq!(cron_mode(false), CronMode::PurgeOnly);
    }

    /// Everything else — `serve`, and a plain `crap-cms work` — evaluates
    /// schedules as before.
    #[test]
    fn cron_is_on_by_default() {
        assert_eq!(cron_mode(true), CronMode::Full);
    }
}
