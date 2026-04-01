//! Background job scheduler: polls for pending jobs, evaluates cron schedules,
//! executes Lua handlers, and manages heartbeats and stale recovery.

mod runner;

pub use runner::{check_cron_schedules, execute_job, purge_soft_deleted, recover_stale_jobs};

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use anyhow::{Context as _, Result, anyhow};
use chrono::Utc;
use tokio::{
    select,
    time::{Duration, interval},
};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::{
    config::{JobsConfig, LocaleConfig},
    core::{SharedRegistry, email::SharedEmailProvider, upload, upload::SharedStorage},
    db::{
        DbConnection, DbPool, DbValue,
        query::{self, images as image_query, jobs as job_query},
    },
    hooks::HookRunner,
};

/// Parameters for starting the scheduler.
pub struct SchedulerParams {
    pool: DbPool,
    hook_runner: HookRunner,
    registry: SharedRegistry,
    config: JobsConfig,
    shutdown: CancellationToken,
    storage: SharedStorage,
    locale_config: LocaleConfig,
    email_provider: Option<SharedEmailProvider>,
    email_queue_timeout: u64,
    email_queue_concurrency: u32,
}

/// Builder for [`SchedulerParams`].
pub struct SchedulerParamsBuilder {
    pool: DbPool,
    hook_runner: HookRunner,
    registry: SharedRegistry,
    config: JobsConfig,
    shutdown: CancellationToken,
    storage: SharedStorage,
    locale_config: LocaleConfig,
    email_provider: Option<SharedEmailProvider>,
    email_queue_timeout: u64,
    email_queue_concurrency: u32,
}

impl SchedulerParamsBuilder {
    pub fn new(
        pool: DbPool,
        hook_runner: HookRunner,
        registry: SharedRegistry,
        config: JobsConfig,
        shutdown: CancellationToken,
        storage: SharedStorage,
        locale_config: LocaleConfig,
    ) -> Self {
        Self {
            pool,
            hook_runner,
            registry,
            config,
            shutdown,
            storage,
            locale_config,
            email_provider: None,
            email_queue_timeout: 30,
            email_queue_concurrency: 5,
        }
    }

    pub fn email_provider(mut self, provider: SharedEmailProvider) -> Self {
        self.email_provider = Some(provider);
        self
    }

    pub fn email_queue_timeout(mut self, timeout: u64) -> Self {
        self.email_queue_timeout = timeout;
        self
    }

    pub fn email_queue_concurrency(mut self, concurrency: u32) -> Self {
        self.email_queue_concurrency = concurrency;
        self
    }

    pub fn build(self) -> SchedulerParams {
        SchedulerParams {
            pool: self.pool,
            hook_runner: self.hook_runner,
            registry: self.registry,
            config: self.config,
            shutdown: self.shutdown,
            storage: self.storage,
            locale_config: self.locale_config,
            email_provider: self.email_provider,
            email_queue_timeout: self.email_queue_timeout,
            email_queue_concurrency: self.email_queue_concurrency,
        }
    }
}

/// Start the scheduler background loop. Runs until the task is cancelled.
// Untestable: infinite async loop with tokio timers and spawn.
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

    // Recover stale jobs and image queue entries on startup
    {
        let conn = pool
            .get()
            .context("Scheduler: failed to get DB connection for recovery")?;
        recover_stale_jobs(&conn, &registry)?;

        match image_query::recover_stale_images(&conn) {
            Ok(n) if n > 0 => info!("Recovered {} stale image queue entries", n),
            Ok(_) => {}
            Err(e) => warn!("Image queue recovery error: {}", e),
        }
    }

    let poll_interval = Duration::from_secs(config.poll_interval);
    let cron_interval = Duration::from_secs(config.cron_interval);
    let heartbeat_interval = Duration::from_secs(config.heartbeat_interval);
    let auto_purge_secs = config.auto_purge;

    let mut poll_ticker = interval(poll_interval);
    let mut cron_ticker = interval(cron_interval);
    let mut heartbeat_ticker = interval(heartbeat_interval);
    // Image processing queue uses the same poll interval as jobs
    let mut image_ticker = interval(poll_interval);

    // Track running job IDs for heartbeat updates
    let running_jobs: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    // Track last cron check time to avoid duplicate firing
    let mut last_cron_check = Utc::now();

    // Auto-purge timer: check once per cron interval
    let mut purge_counter: u64 = 0;

    loop {
        select! {
            _ = shutdown.cancelled() => {
                info!("Scheduler shutting down");
                break Ok(());
            }
            _ = poll_ticker.tick() => {
                // Poll for pending jobs and execute them
                let pool = pool.clone();
                let hook_runner = hook_runner.clone();
                let registry = registry.clone();
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
                    ).await {
                        error!("Scheduler poll error: {}", e);
                    }
                });
            }
            _ = cron_ticker.tick() => {
                // Check cron schedules and insert pending jobs for due schedules
                let now = Utc::now();

                if let Err(e) = check_cron_schedules(&pool, &registry, last_cron_check, now) {
                    error!("Scheduler cron error: {}", e);
                }

                last_cron_check = now;

                // Auto-purge old jobs periodically (every 10 cron intervals)
                purge_counter += 1;

                if purge_counter.is_multiple_of(10)
                    && let Some(secs) = auto_purge_secs
                        && let Ok(conn) = pool.get() {
                            match job_query::purge_old_jobs(&conn, secs) {
                                Ok(n) if n > 0 => info!("Auto-purged {} old job run(s)", n),
                                Ok(_) => {}
                                Err(e) => warn!("Auto-purge error: {}", e),
                            }
                        }

                // Purge expired soft-deleted documents (every 10 cron intervals)
                if purge_counter.is_multiple_of(10)
                    && let Ok(conn) = pool.get() {
                        match purge_soft_deleted(&conn, &registry, &*storage, &locale_config) {
                            Ok(n) if n > 0 => info!("Purged {} expired soft-deleted doc(s)", n),
                            Ok(_) => {}
                            Err(e) => warn!("Soft-delete purge error: {}", e),
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
                                warn!("Heartbeat update error for {}: {}", id, e);
                            }
                        }
                    }
            }
            _ = image_ticker.tick() => {
                // Process pending image format conversions
                let pool = pool.clone();
                let batch_size = config.image_queue_batch_size;
                let img_storage = storage.clone();

                tokio::spawn(async move {
                    if let Err(e) = process_image_queue(&pool, batch_size, &img_storage).await {
                        error!("Image queue error: {}", e);
                    }
                });
            }
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
    let mut conn = pool
        .get()
        .context("Image queue: failed to get DB connection")?;
    let entries = {
        let tx = conn
            .transaction()
            .context("Image queue: failed to begin claim transaction")?;
        let entries = image_query::claim_pending_images(&tx, batch_size)?;
        tx.commit()
            .context("Image queue: failed to commit claim transaction")?;
        entries
    };
    drop(conn);

    for entry in entries {
        let entry_id = entry.id.clone();

        // Validate SQL identifiers from the queue entry before interpolation
        if !query::is_valid_identifier(&entry.collection) {
            tracing::warn!(
                "Image queue: skipping entry {} — invalid collection identifier '{}'",
                entry_id,
                entry.collection
            );
            continue;
        }
        if !query::is_valid_identifier(&entry.url_column) {
            tracing::warn!(
                "Image queue: skipping entry {} — invalid url_column identifier '{}'",
                entry_id,
                entry.url_column
            );
            continue;
        }

        // Process in a blocking task (image encoding is CPU-bound)
        let source = entry.source_path.clone();
        let target = entry.target_path.clone();
        let format = entry.format.clone();
        let quality = entry.quality;
        let img_storage = storage.clone();
        let result = tokio::task::spawn_blocking(move || {
            upload::process_image_entry_with_storage(
                &source,
                &target,
                &format,
                quality,
                &*img_storage,
            )
        })
        .await;

        // Both DB operations (document URL update + queue completion) use the same
        // connection so they succeed or fail together.
        let conn = pool
            .get()
            .context("Image queue: failed to get DB connection")?;
        match result {
            Ok(Ok(())) => {
                conn.execute(
                    &format!(
                        "UPDATE \"{}\" SET \"{}\" = {} WHERE id = {}",
                        entry.collection,
                        entry.url_column,
                        conn.placeholder(1),
                        conn.placeholder(2)
                    ),
                    &[
                        DbValue::Text(entry.url_value.clone()),
                        DbValue::Text(entry.document_id.clone()),
                    ],
                )
                .context("Image queue: failed to update document")?;
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
/// Email queue config passed to poll_and_execute.
struct EmailQueueConfig {
    provider: Option<SharedEmailProvider>,
    timeout: u64,
    concurrency: u32,
}

async fn poll_and_execute(
    pool: &DbPool,
    hook_runner: &HookRunner,
    registry: &SharedRegistry,
    max_concurrent: usize,
    running_jobs: &Arc<Mutex<Vec<String>>>,
    email: &EmailQueueConfig,
) -> Result<()> {
    let mut conn = pool.get().context("Failed to get DB connection")?;

    // Check global concurrency
    let total_running = job_query::count_running(&conn, None)?;

    if total_running as usize >= max_concurrent {
        return Ok(());
    }

    let available = max_concurrent - total_running as usize;

    let job_concurrency = {
        let reg = registry
            .read()
            .map_err(|e| anyhow!("Registry lock poisoned: {}", e))?;
        reg.jobs
            .iter()
            .map(|(slug, def)| (slug.to_string(), def.concurrency))
            .collect::<HashMap<String, u32>>()
    };

    // Claim jobs atomically.
    // SQLite: use IMMEDIATE transaction to serialize writes across workers.
    // Postgres: FOR UPDATE SKIP LOCKED handles concurrency within the query.
    let empty_counts = HashMap::new();
    let claimed = if conn.kind() == "sqlite" {
        let tx = conn
            .transaction_immediate()
            .context("Failed to start claim transaction")?;
        let result =
            job_query::claim_pending_jobs(&tx, available, &empty_counts, &job_concurrency)?;
        tx.commit().context("Failed to commit claim transaction")?;
        result
    } else {
        job_query::claim_pending_jobs(&conn, available, &empty_counts, &job_concurrency)?
    };
    drop(conn);

    for job_run in claimed {
        let job_def = {
            let reg = registry
                .read()
                .map_err(|e| anyhow!("Registry lock poisoned: {}", e))?;
            if let Some(def) = reg.get_job(&job_run.slug) {
                def.clone()
            } else if job_run.slug == crate::core::email::SYSTEM_EMAIL_JOB {
                // System email job: create a synthetic definition from config
                crate::core::job::JobDefinition::builder(
                    crate::core::email::SYSTEM_EMAIL_JOB,
                    "_system",
                )
                .timeout(email.timeout)
                .concurrency(email.concurrency)
                .build()
            } else {
                tracing::warn!(
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
                continue;
            }
        };

        // Track the running job
        if let Ok(mut guard) = running_jobs.lock() {
            guard.push(job_run.id.clone());
        }

        let pool = pool.clone();
        let hook_runner = hook_runner.clone();
        let running_jobs = running_jobs.clone();
        let timeout_secs = job_def.timeout;
        let should_retry = job_run.attempt < job_run.max_attempts;
        let attempt = job_run.attempt;
        let pool_timeout = pool.clone();
        let job_id = job_run.id.clone();
        let id_log = job_run.id.clone();
        let slug_log = job_run.slug.clone();
        let ep = email.provider.clone();

        // Execute the job in a blocking task with enforced timeout.
        // On timeout the blocking thread keeps running (can't cancel sync Rust)
        // but the scheduler immediately marks the job as failed and moves on.
        tokio::spawn(async move {
            let timeout_dur = tokio::time::Duration::from_secs(timeout_secs);
            let result = tokio::time::timeout(
                timeout_dur,
                tokio::task::spawn_blocking(move || {
                    execute_job(&pool, &hook_runner, &job_def, &job_run, ep.as_deref())
                }),
            )
            .await;

            // Always clean up running_jobs tracking
            if let Ok(mut guard) = running_jobs.lock() {
                guard.retain(|id| id != &job_id);
            }

            match result {
                Ok(Ok(Ok(()))) => {}
                Ok(Ok(Err(e))) => {
                    tracing::error!("Job {} ({}) execution error: {}", id_log, slug_log, e);
                }
                Ok(Err(e)) => {
                    tracing::error!("Job {} ({}) panicked: {}", id_log, slug_log, e);
                }
                Err(_) => {
                    tracing::error!(
                        "Job {} ({}) timed out after {}s",
                        id_log,
                        slug_log,
                        timeout_secs
                    );
                    if let Ok(c) = pool_timeout.get() {
                        let _ = job_query::fail_job(
                            &c,
                            &id_log,
                            &format!("timeout after {}s", timeout_secs),
                            should_retry,
                            attempt,
                        );
                    }
                }
            }
        });
    }

    Ok(())
}
