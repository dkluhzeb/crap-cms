//! Scheduler event loop — polls jobs, evaluates cron, processes images, manages heartbeats.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use anyhow::{Context as _, Result};
use chrono::Utc;
use tokio::{
    select,
    time::{Duration, interval},
};
use tracing::{debug, error, info, warn};

use crate::{
    config::LocaleConfig,
    core::{JobDefinition, JobRun, Registry, SharedStorage, email::SYSTEM_EMAIL_JOB, upload},
    db::{
        BoxedConnection, DbConnection, DbPool, DbValue,
        query::{self, images as image_query, jobs as job_query},
    },
    hooks::HookRunner,
};

use super::runner::{
    check_cron_schedules, claim_retention_purge_tick, execute_job, purge_soft_deleted,
    recover_stale_jobs,
};
use super::types::{EmailQueueConfig, SchedulerParams};

/// Start the scheduler background loop. Runs until the cancellation token fires.
///
/// # Errors
///
/// Returns an error if the connection acquisition or stale-job recovery
/// step at startup fails.
#[cfg(not(tarpaulin_include))]
pub async fn start(params: SchedulerParams) -> Result<()> {
    let SchedulerParams {
        pool,
        hook_runner,
        registry,
        config,
        shutdown,
        storage,
        locale_config,
        email_provider,
        email_queue_timeout,
        email_queue_concurrency,
    } = params;

    info!(
        "Scheduler started (poll={}s, cron={}s, max_concurrent={})",
        config.poll_interval, config.cron_interval, config.max_concurrent
    );

    recover_on_startup(&pool, &registry)?;

    let poll_interval = Duration::from_secs(config.poll_interval);
    let cron_interval = Duration::from_secs(config.cron_interval);
    let heartbeat_interval = Duration::from_secs(config.heartbeat_interval);
    let auto_purge_secs = config.auto_purge;

    let mut poll_ticker = interval(poll_interval);
    let mut cron_ticker = interval(cron_interval);
    let mut heartbeat_ticker = interval(heartbeat_interval);
    let mut image_ticker = interval(poll_interval);

    let running_jobs: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let mut last_cron_check = Utc::now();
    let mut purge_counter: u64 = 0;

    loop {
        select! {
            () = shutdown.cancelled() => {
                info!("Scheduler shutting down");
                break Ok(());
            }
            _ = poll_ticker.tick() => {
                let pool = pool.clone();
                let hook_runner = hook_runner.clone();
                let registry = Arc::clone(&registry);
                let running_jobs = running_jobs.clone();
                let max_concurrent = config.max_concurrent;

                let eq = EmailQueueConfig {
                    provider: email_provider.clone(),
                    timeout: email_queue_timeout,
                    concurrency: email_queue_concurrency,
                };

                tokio::spawn(async move {
                    if let Err(e) = poll_and_execute(
                        &pool, &hook_runner, &registry, max_concurrent, &running_jobs, &eq,
                    ) {
                        error!("Scheduler poll error: {}", e);
                    }
                });
            }
            _ = cron_ticker.tick() => {
                let now = Utc::now();

                if let Err(e) = check_cron_schedules(&pool, &registry, last_cron_check, now) {
                    error!("Scheduler cron error: {}", e);
                }

                last_cron_check = now;
                purge_counter += 1;

                run_periodic_purges(&PurgeTickInput {
                    counter: purge_counter,
                    auto_purge_secs,
                    cron_interval_secs: i64::try_from(config.cron_interval).unwrap_or(i64::MAX),
                    pool: &pool,
                    registry: &registry,
                    storage: &storage,
                    locale_config: &locale_config,
                });
            }
            _ = heartbeat_ticker.tick() => {
                update_heartbeats(&pool, &running_jobs);
            }
            _ = image_ticker.tick() => {
                let pool = pool.clone();
                let batch_size = config.image_queue_batch_size;
                let img_storage = storage.clone();

                tokio::spawn(async move {
                    if let Err(e) = process_image_queue(&pool, batch_size, &img_storage).await {
                        // `{:#}` walks the anyhow chain so the SQLite / pool
                        // cause shows up in the log instead of only the
                        // outer "execute failed: UPDATE …" wrapper.
                        error!("Image queue error: {:#}", e);
                    }
                });
            }
        }
    }
}

/// Recover stale jobs and image queue entries on startup.
#[cfg(not(tarpaulin_include))]
fn recover_on_startup(pool: &DbPool, registry: &Registry) -> Result<()> {
    let conn = pool
        .get()
        .context("Scheduler: failed to get DB connection for recovery")?;

    recover_stale_jobs(&conn, registry)?;

    match image_query::recover_stale_images(&conn) {
        Ok(n) if n > 0 => info!("Recovered {} stale image queue entries", n),
        Ok(_) => {}
        Err(e) => warn!("Image queue recovery error: {}", e),
    }

    Ok(())
}

/// Bundle of refs needed by the periodic-purge tick. Bundled into a
/// struct so the call site reads at a glance instead of counting
/// positional arguments.
#[cfg(not(tarpaulin_include))]
struct PurgeTickInput<'a> {
    counter: u64,
    auto_purge_secs: Option<u64>,
    cron_interval_secs: i64,
    pool: &'a DbPool,
    registry: &'a Registry,
    storage: &'a SharedStorage,
    locale_config: &'a LocaleConfig,
}

/// Run periodic purges (every 10 cron intervals).
///
/// In multi-node deployments the retention purge is gated by an atomic
/// `_crap_cron_fired` claim -- only one node runs the purge per cron window.
#[cfg(not(tarpaulin_include))]
fn run_periodic_purges(p: &PurgeTickInput<'_>) {
    if !p.counter.is_multiple_of(10) {
        return;
    }

    if let Some(secs) = p.auto_purge_secs
        && let Ok(conn) = p.pool.get()
    {
        match job_query::purge_old_jobs(&conn, secs) {
            Ok(n) if n > 0 => info!("Auto-purged {} old job run(s)", n),
            Ok(_) => {}
            Err(e) => warn!("Auto-purge error: {}", e),
        }
    }

    let Ok(mut conn) = p.pool.get() else {
        return;
    };

    // The purge fires every 10 cron intervals, so the dedup window must cover
    // that span -- otherwise two nodes drifting by ~1 cron tick would each
    // claim a fresh window and run the purge twice.
    let purge_window_secs = p.cron_interval_secs.saturating_mul(10);

    let claimed = match conn.transaction() {
        Ok(tx) => match claim_retention_purge_tick(&tx, Utc::now(), purge_window_secs) {
            Ok(true) => match tx.commit() {
                Ok(()) => true,
                Err(e) => {
                    warn!("Failed to commit retention-purge claim: {}", e);
                    false
                }
            },
            Ok(false) => {
                debug!("Retention purge already claimed by another instance this window");
                false
            }
            Err(e) => {
                warn!("Retention-purge claim error: {}", e);
                false
            }
        },
        Err(e) => {
            warn!(
                "Failed to open transaction for retention-purge claim: {}",
                e
            );
            false
        }
    };

    if !claimed {
        return;
    }

    match purge_soft_deleted(&conn, p.registry, &**p.storage, p.locale_config) {
        Ok(n) if n > 0 => info!("Purged {} expired soft-deleted doc(s)", n),
        Ok(_) => {}
        Err(e) => warn!("Soft-delete purge error: {}", e),
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

    let Ok(conn) = pool.get() else { return };

    for id in &ids {
        if let Err(e) = job_query::update_heartbeat(&conn, id) {
            warn!("Heartbeat update error for {}: {}", id, e);
        }
    }
}

/// Process pending image format conversions from the queue.
#[cfg(not(tarpaulin_include))]
async fn process_image_queue(
    pool: &DbPool,
    batch_size: usize,
    storage: &SharedStorage,
) -> Result<()> {
    let entries = claim_image_batch(pool, batch_size)?;

    for entry in entries {
        process_single_image(pool, &entry, storage).await?;
    }

    Ok(())
}

/// Claim a batch of pending image entries atomically.
#[cfg(not(tarpaulin_include))]
fn claim_image_batch(
    pool: &DbPool,
    batch_size: usize,
) -> Result<Vec<image_query::ImageQueueEntry>> {
    let mut conn = pool
        .get()
        .context("Image queue: failed to get DB connection")?;
    let tx = conn
        .transaction()
        .context("Image queue: failed to begin claim transaction")?;
    let entries = image_query::claim_pending_images(&tx, batch_size)?;
    tx.commit()
        .context("Image queue: failed to commit claim transaction")?;

    Ok(entries)
}

/// Process a single image queue entry: convert format and update DB.
#[cfg(not(tarpaulin_include))]
async fn process_single_image(
    pool: &DbPool,
    entry: &image_query::ImageQueueEntry,
    storage: &SharedStorage,
) -> Result<()> {
    let entry_id = entry.id.clone();

    if !query::is_valid_identifier(&entry.collection) {
        warn!(
            "Image queue: skipping entry {} — invalid collection '{}'",
            entry_id, entry.collection
        );
        return Ok(());
    }
    if !query::is_valid_identifier(&entry.url_column) {
        warn!(
            "Image queue: skipping entry {} — invalid url_column '{}'",
            entry_id, entry.url_column
        );
        return Ok(());
    }

    let source = entry.source_path.clone();
    let target = entry.target_path.clone();
    let format = entry.format.clone();
    let quality = entry.quality;
    let img_storage = storage.clone();

    let result = tokio::task::spawn_blocking(move || {
        upload::process_image_entry_with_storage(&source, &target, &format, quality, &*img_storage)
    })
    .await;

    let mut conn = pool
        .get()
        .context("Image queue: failed to get DB connection")?;

    match result {
        Ok(Ok(())) => {
            // Atomically: write the new URL into the collection doc AND mark
            // the queue entry completed. Leaving this as two sequential
            // statements would orphan the queue row in `processing` whenever
            // the second write hit `SQLITE_BUSY` after the first succeeded.
            let commit = record_conversion_success(&mut conn, entry, &entry_id);

            match commit {
                Ok(()) => debug!("Image conversion completed: {}", entry_id),
                Err(e) => {
                    // The file is on disk and valid, but the DB commit lost.
                    // Mark the row failed so startup / retry can pick it back
                    // up; never leave it stuck in `processing`.
                    warn!(
                        "Image queue: DB commit failed after successful conversion {}: {:#}",
                        entry_id, e
                    );

                    if let Err(fe) = image_query::fail_image_entry(
                        &conn,
                        &entry_id,
                        &format!("db commit failed: {e}"),
                    ) {
                        error!(
                            "Image queue: failed to record failure for {}: {:#}",
                            entry_id, fe
                        );
                    }
                }
            }
        }
        Ok(Err(e)) => {
            warn!("Image conversion failed: {}: {:#}", entry_id, e);
            image_query::fail_image_entry(&conn, &entry_id, &e.to_string())?;
        }
        Err(e) => {
            error!("Image conversion panicked: {}: {}", entry_id, e);
            image_query::fail_image_entry(&conn, &entry_id, &format!("panic: {e}"))?;
        }
    }

    Ok(())
}

/// Atomically update the target document's URL column and mark the queue
/// entry completed. Wrapping both writes in a single transaction guarantees
/// the queue row never stays in `processing` while the document shows the
/// new URL (or vice-versa) — on any write failure the whole thing rolls back
/// and the caller can mark the entry failed for later retry.
fn record_conversion_success(
    conn: &mut crate::db::BoxedConnection,
    entry: &image_query::ImageQueueEntry,
    entry_id: &str,
) -> Result<()> {
    let tx = conn
        .transaction()
        .context("Image queue: failed to begin completion transaction")?;

    tx.execute(
        &format!(
            "UPDATE \"{}\" SET \"{}\" = {} WHERE id = {}",
            entry.collection,
            entry.url_column,
            tx.placeholder(1),
            tx.placeholder(2)
        ),
        &[
            DbValue::Text(entry.url_value.clone()),
            DbValue::Text(entry.document_id.clone()),
        ],
    )
    .context("Image queue: failed to update document")?;

    image_query::complete_image_entry(&tx, entry_id)
        .context("Image queue: failed to mark entry completed")?;

    tx.commit()
        .context("Image queue: failed to commit completion transaction")?;

    Ok(())
}

/// Poll for pending jobs and execute them.
#[cfg(not(tarpaulin_include))]
fn poll_and_execute(
    pool: &DbPool,
    hook_runner: &HookRunner,
    registry: &Registry,
    max_concurrent: usize,
    running_jobs: &Arc<Mutex<Vec<String>>>,
    email: &EmailQueueConfig,
) -> Result<()> {
    let mut conn = pool.get().context("Failed to get DB connection")?;

    let total_running = job_query::count_running(&conn, None)?;
    // Saturate to max_concurrent so a runaway counter still gates new jobs
    // (zero `available` = skip this tick rather than over-claiming).
    let running_usize = usize::try_from(total_running).unwrap_or(max_concurrent);
    if running_usize >= max_concurrent {
        return Ok(());
    }

    let available = max_concurrent - running_usize;
    let job_concurrency = read_job_concurrency(registry);

    let empty_counts = HashMap::new();
    let claimed = claim_pending_jobs(&mut conn, available, &empty_counts, &job_concurrency)?;
    drop(conn);

    for job_run in claimed {
        let Some(job_def) = resolve_job_def(registry, &job_run, pool, email) else {
            continue;
        };

        spawn_job_execution(&SpawnJobInput {
            pool,
            hook_runner,
            running_jobs,
            email,
            job_run: &job_run,
            job_def: &job_def,
        });
    }

    Ok(())
}

/// Read per-slug concurrency limits from the registry.
#[cfg(not(tarpaulin_include))]
fn read_job_concurrency(registry: &Registry) -> HashMap<String, u32> {
    registry
        .jobs
        .iter()
        .map(|(slug, def)| (slug.to_string(), def.concurrency))
        .collect()
}

/// Claim pending jobs, using IMMEDIATE transaction for `SQLite`.
#[cfg(not(tarpaulin_include))]
fn claim_pending_jobs(
    conn: &mut BoxedConnection,
    available: usize,
    running_counts: &HashMap<String, i64>,
    job_concurrency: &HashMap<String, u32>,
) -> Result<Vec<JobRun>> {
    if conn.kind() == "sqlite" {
        let tx = conn
            .transaction_immediate()
            .context("Failed to start claim transaction")?;
        let result =
            job_query::claim_pending_jobs(&tx, available, running_counts, job_concurrency)?;
        tx.commit().context("Failed to commit claim transaction")?;
        Ok(result)
    } else {
        Ok(job_query::claim_pending_jobs(
            conn,
            available,
            running_counts,
            job_concurrency,
        )?)
    }
}

/// Resolve the job definition for a claimed job run.
#[cfg(not(tarpaulin_include))]
fn resolve_job_def(
    registry: &Registry,
    job_run: &JobRun,
    pool: &DbPool,
    email: &EmailQueueConfig,
) -> Option<JobDefinition> {
    if let Some(def) = registry.get_job(&job_run.slug) {
        return Some(def.clone());
    }

    if job_run.slug == SYSTEM_EMAIL_JOB {
        return Some(
            JobDefinition::builder(SYSTEM_EMAIL_JOB, "_system")
                .timeout(email.timeout)
                .concurrency(email.concurrency)
                .build(),
        );
    }

    warn!(
        "Job definition '{}' not found, marking as failed",
        job_run.slug
    );

    if let Ok(c) = pool.get() {
        let _ = job_query::fail_job(
            &c,
            &job_run.id,
            "job definition not found",
            false,
            job_run.attempt,
        );
    }

    None
}

struct SpawnJobInput<'a> {
    pool: &'a DbPool,
    hook_runner: &'a HookRunner,
    running_jobs: &'a Arc<Mutex<Vec<String>>>,
    email: &'a EmailQueueConfig,
    job_run: &'a JobRun,
    job_def: &'a JobDefinition,
}

/// Spawn a tokio task to execute a job with timeout enforcement.
#[cfg(not(tarpaulin_include))]
fn spawn_job_execution(s: &SpawnJobInput<'_>) {
    if let Ok(mut guard) = s.running_jobs.lock() {
        guard.push(s.job_run.id.clone());
    }

    let pool = s.pool.clone();
    let hook_runner = s.hook_runner.clone();
    let running_jobs = s.running_jobs.clone();
    let timeout_secs = s.job_def.timeout;
    let should_retry = s.job_run.attempt < s.job_run.max_attempts;
    let attempt = s.job_run.attempt;
    let pool_timeout = pool.clone();
    let job_id = s.job_run.id.clone();
    let id_log = s.job_run.id.clone();
    let slug_log = s.job_run.slug.clone();
    let ep = s.email.provider.clone();
    let job_def = s.job_def.clone();
    let job_run = s.job_run.clone();

    tokio::spawn(async move {
        let timeout_dur = Duration::from_secs(timeout_secs);
        let result = tokio::time::timeout(
            timeout_dur,
            tokio::task::spawn_blocking(move || {
                execute_job(&pool, &hook_runner, &job_def, &job_run, ep.as_deref())
            }),
        )
        .await;

        if let Ok(mut guard) = running_jobs.lock() {
            guard.retain(|id| id != &job_id);
        }

        match result {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(e))) => error!("Job {} ({}) execution error: {}", id_log, slug_log, e),
            Ok(Err(e)) => error!("Job {} ({}) panicked: {}", id_log, slug_log, e),
            Err(_) => {
                error!(
                    "Job {} ({}) timed out after {}s",
                    id_log, slug_log, timeout_secs
                );

                if let Ok(c) = pool_timeout.get() {
                    let _ = job_query::fail_job(
                        &c,
                        &id_log,
                        &format!("timeout after {timeout_secs}s"),
                        should_retry,
                        attempt,
                    );
                }
            }
        }
    });
}
