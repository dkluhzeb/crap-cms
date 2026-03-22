//! Background job scheduler: polls for pending jobs, evaluates cron schedules,
//! executes Lua handlers, and manages heartbeats and stale recovery.

mod runner;

pub use runner::{check_cron_schedules, execute_job, recover_stale_jobs};

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use anyhow::{Context as _, Result, anyhow};
use tokio::select;

use crate::{
    config::JobsConfig,
    core::{SharedRegistry, upload},
    db::{
        DbConnection, DbPool,
        query::{images as image_query, jobs as job_query},
    },
    hooks::HookRunner,
};

/// Start the scheduler background loop. Runs until the task is cancelled.
// Untestable: infinite async loop with tokio timers and spawn.
#[cfg(not(tarpaulin_include))]
pub async fn start(
    pool: DbPool,
    hook_runner: HookRunner,
    registry: SharedRegistry,
    config: JobsConfig,
    shutdown: tokio_util::sync::CancellationToken,
) -> Result<()> {
    tracing::info!(
        "Scheduler started (poll={}s, cron={}s, max_concurrent={})",
        config.poll_interval,
        config.cron_interval,
        config.max_concurrent
    );

    // Recover stale jobs and image queue entries on startup
    {
        let conn = pool
            .get()
            .context("Scheduler: failed to get DB connection for recovery")?;
        recover_stale_jobs(&conn, &registry)?;

        match image_query::recover_stale_images(&conn) {
            Ok(n) if n > 0 => tracing::info!("Recovered {} stale image queue entries", n),
            Ok(_) => {}
            Err(e) => tracing::warn!("Image queue recovery error: {}", e),
        }
    }

    let poll_interval = tokio::time::Duration::from_secs(config.poll_interval);
    let cron_interval = tokio::time::Duration::from_secs(config.cron_interval);
    let heartbeat_interval = tokio::time::Duration::from_secs(config.heartbeat_interval);
    let auto_purge_secs = config.auto_purge;

    let mut poll_ticker = tokio::time::interval(poll_interval);
    let mut cron_ticker = tokio::time::interval(cron_interval);
    let mut heartbeat_ticker = tokio::time::interval(heartbeat_interval);
    // Image processing queue uses the same poll interval as jobs
    let mut image_ticker = tokio::time::interval(poll_interval);

    // Track running job IDs for heartbeat updates
    let running_jobs: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    // Track last cron check time to avoid duplicate firing
    let mut last_cron_check = chrono::Utc::now();

    // Auto-purge timer: check once per cron interval
    let mut purge_counter: u64 = 0;

    loop {
        select! {
            _ = shutdown.cancelled() => {
                tracing::info!("Scheduler shutting down");
                break Ok(());
            }
            _ = poll_ticker.tick() => {
                // Poll for pending jobs and execute them
                let pool = pool.clone();
                let hook_runner = hook_runner.clone();
                let registry = registry.clone();
                let running_jobs = running_jobs.clone();
                let max_concurrent = config.max_concurrent;

                tokio::spawn(async move {
                    if let Err(e) = poll_and_execute(
                        &pool, &hook_runner, &registry, max_concurrent, &running_jobs,
                    ).await {
                        tracing::error!("Scheduler poll error: {}", e);
                    }
                });
            }
            _ = cron_ticker.tick() => {
                // Check cron schedules and insert pending jobs for due schedules
                let now = chrono::Utc::now();

                if let Err(e) = check_cron_schedules(&pool, &registry, last_cron_check, now) {
                    tracing::error!("Scheduler cron error: {}", e);
                }
                last_cron_check = now;

                // Auto-purge old jobs periodically (every 10 cron intervals)
                purge_counter += 1;

                if purge_counter.is_multiple_of(10)
                    && let Some(secs) = auto_purge_secs
                        && let Ok(conn) = pool.get() {
                            match job_query::purge_old_jobs(&conn, secs) {
                                Ok(n) if n > 0 => tracing::info!("Auto-purged {} old job run(s)", n),
                                Ok(_) => {}
                                Err(e) => tracing::warn!("Auto-purge error: {}", e),
                            }
                        }
            }
            _ = heartbeat_ticker.tick() => {
                // Update heartbeats for all running jobs
                let ids: Vec<String> = running_jobs.lock()
                    .map(|guard| guard.clone())
                    .unwrap_or_default();

                if !ids.is_empty()
                    && let Ok(conn) = pool.get() {
                        for id in &ids {
                            if let Err(e) = job_query::update_heartbeat(&conn, id) {
                                tracing::warn!("Heartbeat update error for {}: {}", id, e);
                            }
                        }
                    }
            }
            _ = image_ticker.tick() => {
                // Process pending image format conversions
                let pool = pool.clone();
                let batch_size = config.image_queue_batch_size;
                tokio::spawn(async move {
                    if let Err(e) = process_image_queue(&pool, batch_size).await {
                        tracing::error!("Image queue error: {}", e);
                    }
                });
            }
        }
    }
}

/// Process pending image format conversions from the queue.
#[cfg(not(tarpaulin_include))]
async fn process_image_queue(pool: &DbPool, batch_size: usize) -> Result<()> {
    let conn = pool
        .get()
        .context("Image queue: failed to get DB connection")?;
    let entries = image_query::claim_pending_images(&conn, batch_size)?;
    drop(conn);

    for entry in entries {
        let pool_inner = pool.clone();
        let entry_id = entry.id.clone();

        // Process in a blocking task (image encoding is CPU-bound)
        let result = tokio::task::spawn_blocking(move || {
            let pool = pool_inner;
            upload::process_image_entry(
                &entry.source_path,
                &entry.target_path,
                &entry.format,
                entry.quality,
            )?;

            // Update the document's format URL column
            let conn = pool
                .get()
                .context("Image queue: failed to get DB connection")?;
            conn.execute(
                &format!(
                    "UPDATE \"{}\" SET \"{}\" = {} WHERE id = {}",
                    entry.collection,
                    entry.url_column,
                    conn.placeholder(1),
                    conn.placeholder(2)
                ),
                &[
                    crate::db::DbValue::Text(entry.url_value.clone()),
                    crate::db::DbValue::Text(entry.document_id.clone()),
                ],
            )
            .context("Image queue: failed to update document")?;

            Ok::<(), anyhow::Error>(())
        })
        .await;

        let conn = pool
            .get()
            .context("Image queue: failed to get DB connection")?;
        match result {
            Ok(Ok(())) => {
                image_query::complete_image_entry(&conn, &entry_id)?;
                tracing::debug!("Image conversion completed: {}", entry_id);
            }
            Ok(Err(e)) => {
                tracing::warn!("Image conversion failed: {}: {}", entry_id, e);
                image_query::fail_image_entry(&conn, &entry_id, &e.to_string())?;
            }
            Err(e) => {
                tracing::error!("Image conversion panicked: {}: {}", entry_id, e);
                image_query::fail_image_entry(&conn, &entry_id, &format!("panic: {}", e))?;
            }
        }
    }

    Ok(())
}

/// Poll for pending jobs and execute them.
// Untestable: async function with tokio::task::spawn_blocking orchestration.
#[cfg(not(tarpaulin_include))]
async fn poll_and_execute(
    pool: &DbPool,
    hook_runner: &HookRunner,
    registry: &SharedRegistry,
    max_concurrent: usize,
    running_jobs: &Arc<Mutex<Vec<String>>>,
) -> Result<()> {
    let conn = pool.get().context("Failed to get DB connection")?;

    // Check global concurrency
    let total_running = job_query::count_running(&conn, None)?;

    if total_running as usize >= max_concurrent {
        return Ok(());
    }

    let available = max_concurrent - total_running as usize;

    // Get per-slug running counts and concurrency limits
    let running_counts = job_query::count_running_per_slug(&conn)?;
    let job_concurrency = {
        let reg = registry
            .read()
            .map_err(|e| anyhow!("Registry lock poisoned: {}", e))?;
        reg.jobs
            .iter()
            .map(|(slug, def)| (slug.to_string(), def.concurrency))
            .collect::<HashMap<String, u32>>()
    };

    let claimed =
        job_query::claim_pending_jobs(&conn, available, &running_counts, &job_concurrency)?;
    drop(conn);

    for job_run in claimed {
        let job_def = {
            let reg = registry
                .read()
                .map_err(|e| anyhow!("Registry lock poisoned: {}", e))?;
            match reg.get_job(&job_run.slug) {
                Some(def) => def.clone(),
                None => {
                    tracing::warn!(
                        "Job definition '{}' not found, marking as failed",
                        job_run.slug
                    );

                    if let Ok(c) = pool.get() {
                        let _ =
                            job_query::fail_job(&c, &job_run.id, "job definition not found", false);
                    }
                    continue;
                }
            }
        };

        // Track the running job
        if let Ok(mut guard) = running_jobs.lock() {
            guard.push(job_run.id.clone());
        }

        let pool = pool.clone();
        let hook_runner = hook_runner.clone();
        let running_jobs = running_jobs.clone();
        let job_id = job_run.id.clone();

        // Execute the job in a blocking task (same pattern as hook execution).
        // Wrapped in tokio::spawn to catch panics from spawn_blocking.
        let slug_log = job_run.slug.clone();
        let id_log = job_run.id.clone();
        tokio::spawn(async move {
            match tokio::task::spawn_blocking(move || {
                let result = execute_job(&pool, &hook_runner, &job_def, &job_run);

                // Remove from running tracking
                if let Ok(mut guard) = running_jobs.lock() {
                    guard.retain(|id| id != &job_id);
                }

                if let Err(e) = result {
                    tracing::error!("Job {} ({}) execution error: {}", job_id, job_run.slug, e);
                }
            })
            .await
            {
                Ok(()) => {}
                Err(e) => tracing::error!("Job {} ({}) panicked: {}", id_log, slug_log, e),
            }
        });
    }

    Ok(())
}
