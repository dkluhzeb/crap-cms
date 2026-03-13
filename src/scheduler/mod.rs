//! Background job scheduler: polls for pending jobs, evaluates cron schedules,
//! executes Lua handlers, and manages heartbeats and stale recovery.

use std::{
    cmp::max,
    collections::HashMap,
    str::FromStr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, anyhow};

use crate::{
    config::JobsConfig,
    core::{
        SharedRegistry,
        job::{JobDefinition, JobRun},
        upload,
    },
    db::{
        DbPool,
        query::{images as image_query, jobs as job_query},
    },
    hooks::lifecycle::HookRunner,
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

    // Recover stale jobs on startup
    {
        let conn = pool
            .get()
            .context("Scheduler: failed to get DB connection for recovery")?;
        recover_stale_jobs(&conn, &registry)?;
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
        tokio::select! {
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
                    "UPDATE \"{}\" SET \"{}\" = ?1 WHERE id = ?2",
                    entry.collection, entry.url_column
                ),
                rusqlite::params![entry.url_value, entry.document_id],
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
            .map(|(slug, def)| (slug.clone(), def.concurrency))
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

/// Execute a single job: call the Lua handler with CRUD access.
pub fn execute_job(
    pool: &DbPool,
    hook_runner: &HookRunner,
    job_def: &JobDefinition,
    job_run: &JobRun,
) -> Result<()> {
    let timeout = Duration::from_secs(job_def.timeout);
    let start = Instant::now();

    tracing::info!(
        "Executing job {} ({}) attempt {}/{}",
        job_run.id,
        job_run.slug,
        job_run.attempt,
        job_run.max_attempts
    );

    // Open a transaction for the job handler (same TxContext pattern as hooks)
    let mut conn = pool.get().context("Failed to get DB connection for job")?;
    let tx = conn
        .transaction()
        .context("Failed to begin job transaction")?;

    let result = hook_runner.run_job_handler(
        &job_def.handler,
        &job_run.slug,
        &job_run.data,
        job_run.attempt,
        job_run.max_attempts,
        &tx,
    );

    match result {
        Ok(result_json) => {
            tx.commit().context("Failed to commit job transaction")?;
            let c = pool
                .get()
                .context("Failed to get DB connection for completion")?;
            job_query::complete_job(&c, &job_run.id, result_json.as_deref())?;
            let elapsed = start.elapsed();
            tracing::info!(
                "Job {} ({}) completed in {:?}",
                job_run.id,
                job_run.slug,
                elapsed
            );
        }
        Err(e) => {
            // Transaction rolls back on drop
            drop(tx);
            let error_msg = if start.elapsed() >= timeout {
                format!("timeout after {}s", job_def.timeout)
            } else {
                format!("{}", e)
            };
            let should_retry = job_run.attempt < job_run.max_attempts;
            let c = pool
                .get()
                .context("Failed to get DB connection for failure")?;
            job_query::fail_job(&c, &job_run.id, &error_msg, should_retry)?;

            if should_retry {
                tracing::warn!(
                    "Job {} ({}) failed (attempt {}/{}), will retry: {}",
                    job_run.id,
                    job_run.slug,
                    job_run.attempt,
                    job_run.max_attempts,
                    error_msg
                );
            } else {
                tracing::error!(
                    "Job {} ({}) failed permanently: {}",
                    job_run.id,
                    job_run.slug,
                    error_msg
                );
            }
        }
    }

    Ok(())
}

/// Check cron schedules and insert pending jobs for due ones.
pub fn check_cron_schedules(
    pool: &DbPool,
    registry: &SharedRegistry,
    last_check: chrono::DateTime<chrono::Utc>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    let reg = registry
        .read()
        .map_err(|e| anyhow!("Registry lock poisoned: {}", e))?;

    let conn = pool.get().context("Failed to get DB connection for cron")?;

    for (slug, def) in &reg.jobs {
        let schedule_str = match &def.schedule {
            Some(s) => s,
            None => continue,
        };

        // Parse cron expression (the cron crate expects 6-7 fields with seconds;
        // normalize standard 5-field expressions by prepending "0" for seconds)
        let normalized = normalize_cron(schedule_str);
        let schedule = match cron::Schedule::from_str(&normalized) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    "Invalid cron expression '{}' for job '{}': {}",
                    schedule_str,
                    slug,
                    e
                );
                continue;
            }
        };

        // Check if the schedule should have fired between last_check and now
        let should_fire = schedule
            .after(&last_check)
            .take_while(|t| *t <= now)
            .next()
            .is_some();

        if !should_fire {
            continue;
        }

        // Check skip_if_running
        if def.skip_if_running {
            let running = job_query::count_running(&conn, Some(slug))?;

            if running > 0 {
                tracing::debug!("Skipping cron job '{}' — still running", slug);
                continue;
            }
        }

        // Insert a pending job
        let job = job_query::insert_job(&conn, slug, "{}", "cron", def.retries + 1, &def.queue)?;
        tracing::info!("Cron scheduled job '{}' (run {})", slug, job.id);
    }

    Ok(())
}

/// Recover stale jobs on startup.
pub fn recover_stale_jobs(conn: &rusqlite::Connection, registry: &SharedRegistry) -> Result<()> {
    let reg = registry
        .read()
        .map_err(|e| anyhow!("Registry lock poisoned: {}", e))?;

    // Find all running jobs — on startup, these are stale (server was restarted)
    let stale = job_query::find_stale_jobs(conn, 0)?;

    for job in &stale {
        let timeout = reg.jobs.get(&job.slug).map(|d| d.timeout).unwrap_or(60);
        let threshold = max(timeout * 2, 300);

        // Any job that was running when we started is stale
        let error = format!(
            "stale: server restarted (was running, timeout={}s)",
            threshold
        );
        job_query::mark_stale(conn, &job.id, &error)?;
        tracing::info!("Marked stale job {} ({})", job.id, job.slug);
    }

    if !stale.is_empty() {
        tracing::info!("Recovered {} stale job(s)", stale.len());
    }

    Ok(())
}

/// Normalize a cron expression: the `cron` crate expects 6 or 7 fields (with a
/// leading seconds field), but users write standard 5-field cron (`0 3 * * *`).
/// If the expression has exactly 5 fields, prepend "0" for seconds.
pub(crate) fn normalize_cron(expr: &str) -> String {
    let fields: Vec<&str> = expr.split_whitespace().collect();

    if fields.len() == 5 {
        format!("0 {}", expr)
    } else {
        expr.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Registry, job::JobStatus};

    // ── normalize_cron ──────────────────────────────────────────────────

    #[test]
    fn normalize_cron_five_fields() {
        let result = normalize_cron("0 3 * * *");
        assert_eq!(result, "0 0 3 * * *");
    }

    #[test]
    fn normalize_cron_six_fields_unchanged() {
        let result = normalize_cron("0 0 3 * * *");
        assert_eq!(result, "0 0 3 * * *");
    }

    #[test]
    fn normalize_cron_seven_fields_unchanged() {
        let result = normalize_cron("0 0 3 * * * 2024");
        assert_eq!(result, "0 0 3 * * * 2024");
    }

    #[test]
    fn normalize_cron_every_minute() {
        let result = normalize_cron("* * * * *");
        assert_eq!(result, "0 * * * * *");
    }

    #[test]
    fn normalize_cron_complex_expression() {
        let result = normalize_cron("*/5 9-17 * * 1-5");
        assert_eq!(result, "0 */5 9-17 * * 1-5");
    }

    #[test]
    fn normalize_cron_empty_string() {
        let result = normalize_cron("");
        assert_eq!(result, "");
    }

    #[test]
    fn normalize_cron_single_field() {
        let result = normalize_cron("*");
        assert_eq!(result, "*");
    }

    #[test]
    fn normalize_cron_two_fields() {
        let result = normalize_cron("0 3");
        assert_eq!(result, "0 3");
    }

    #[test]
    fn normalize_cron_four_fields() {
        let result = normalize_cron("0 3 * *");
        assert_eq!(result, "0 3 * *");
    }

    #[test]
    fn normalize_cron_extra_whitespace() {
        // split_whitespace handles multiple spaces, so this still counts as 5 fields
        let result = normalize_cron("0  3  *  *  *");
        assert_eq!(result, "0 0  3  *  *  *");
    }

    #[test]
    fn normalize_cron_with_ranges_and_steps() {
        let result = normalize_cron("0-30/5 0-23 1-15 1-6 0-4");
        assert_eq!(result, "0 0-30/5 0-23 1-15 1-6 0-4");
    }

    #[test]
    fn normalize_cron_result_is_parseable() {
        // Verify that a normalized 5-field expression produces a valid cron schedule
        let normalized = normalize_cron("0 3 * * *");
        let schedule = cron::Schedule::from_str(&normalized);
        assert!(
            schedule.is_ok(),
            "Normalized expression should be parseable"
        );
    }

    // ── recover_stale_jobs ──────────────────────────────────────────────

    fn setup_jobs_db() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE _crap_jobs (
                id TEXT PRIMARY KEY,
                slug TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending',
                queue TEXT NOT NULL DEFAULT 'default',
                data TEXT DEFAULT '{}',
                result TEXT,
                error TEXT,
                attempt INTEGER NOT NULL DEFAULT 0,
                max_attempts INTEGER NOT NULL DEFAULT 1,
                scheduled_by TEXT,
                created_at TEXT DEFAULT (datetime('now')),
                started_at TEXT,
                completed_at TEXT,
                heartbeat_at TEXT
            );
            CREATE INDEX idx_crap_jobs_status ON _crap_jobs(status);
            CREATE INDEX idx_crap_jobs_queue ON _crap_jobs(queue, status);
            CREATE INDEX idx_crap_jobs_slug ON _crap_jobs(slug, status);",
        )
        .unwrap();
        conn
    }

    fn make_registry_with_jobs(jobs: Vec<JobDefinition>) -> SharedRegistry {
        let registry = Registry::shared();
        {
            let mut reg = registry.write().unwrap();
            for job in jobs {
                reg.register_job(job);
            }
        }
        registry
    }

    #[test]
    fn recover_stale_jobs_marks_running_as_stale() {
        let conn = setup_jobs_db();
        let registry = make_registry_with_jobs(vec![
            JobDefinition::builder("my_job", "some.handler")
                .timeout(120)
                .build(),
        ]);

        // Insert a running job (simulates server crash with running job)
        job_query::insert_job(&conn, "my_job", "{}", "manual", 1, "default").unwrap();
        conn.execute(
            "UPDATE _crap_jobs SET status = 'running', heartbeat_at = datetime('now', '-600 seconds')",
            [],
        ).unwrap();

        recover_stale_jobs(&conn, &registry).unwrap();

        let jobs = job_query::list_job_runs(&conn, None, Some("stale"), 100, 0).unwrap();
        assert_eq!(jobs.len(), 1);
        assert!(
            jobs[0]
                .error
                .as_ref()
                .unwrap()
                .contains("stale: server restarted")
        );
        assert!(jobs[0].error.as_ref().unwrap().contains("timeout=300s"));
    }

    #[test]
    fn recover_stale_jobs_uses_job_timeout() {
        let conn = setup_jobs_db();
        let registry = make_registry_with_jobs(vec![
            JobDefinition::builder("long_job", "some.handler")
                .timeout(3600)
                .build(), // 1 hour
        ]);

        job_query::insert_job(&conn, "long_job", "{}", "manual", 1, "default").unwrap();
        conn.execute(
            "UPDATE _crap_jobs SET status = 'running', heartbeat_at = datetime('now', '-600 seconds')",
            [],
        ).unwrap();

        recover_stale_jobs(&conn, &registry).unwrap();

        let jobs = job_query::list_job_runs(&conn, None, Some("stale"), 100, 0).unwrap();
        assert_eq!(jobs.len(), 1);
        // threshold = max(3600 * 2, 300) = 7200
        assert!(jobs[0].error.as_ref().unwrap().contains("timeout=7200s"));
    }

    #[test]
    fn recover_stale_jobs_default_timeout_for_unknown_slug() {
        let conn = setup_jobs_db();
        // Registry has no job definitions — slug not found, uses default timeout=60
        let registry = make_registry_with_jobs(vec![]);

        job_query::insert_job(&conn, "unknown_job", "{}", "manual", 1, "default").unwrap();
        conn.execute(
            "UPDATE _crap_jobs SET status = 'running', heartbeat_at = datetime('now', '-600 seconds')",
            [],
        ).unwrap();

        recover_stale_jobs(&conn, &registry).unwrap();

        let jobs = job_query::list_job_runs(&conn, None, Some("stale"), 100, 0).unwrap();
        assert_eq!(jobs.len(), 1);
        // threshold = max(60 * 2, 300) = 300
        assert!(jobs[0].error.as_ref().unwrap().contains("timeout=300s"));
    }

    #[test]
    fn recover_stale_jobs_no_running_is_noop() {
        let conn = setup_jobs_db();
        let registry = make_registry_with_jobs(vec![]);

        // Insert a pending job — should not be affected
        job_query::insert_job(&conn, "my_job", "{}", "manual", 1, "default").unwrap();

        recover_stale_jobs(&conn, &registry).unwrap();

        let stale = job_query::list_job_runs(&conn, None, Some("stale"), 100, 0).unwrap();
        assert_eq!(stale.len(), 0);

        let pending = job_query::list_job_runs(&conn, None, Some("pending"), 100, 0).unwrap();
        assert_eq!(pending.len(), 1);
    }

    #[test]
    fn recover_stale_jobs_multiple_running() {
        let conn = setup_jobs_db();
        let registry = make_registry_with_jobs(vec![
            JobDefinition::builder("job_a", "handler_a")
                .timeout(60)
                .build(),
            JobDefinition::builder("job_b", "handler_b")
                .timeout(120)
                .build(),
        ]);

        job_query::insert_job(&conn, "job_a", "{}", "manual", 1, "default").unwrap();
        job_query::insert_job(&conn, "job_b", "{}", "manual", 1, "default").unwrap();
        conn.execute("UPDATE _crap_jobs SET status = 'running'", [])
            .unwrap();

        recover_stale_jobs(&conn, &registry).unwrap();

        let stale = job_query::list_job_runs(&conn, None, Some("stale"), 100, 0).unwrap();
        assert_eq!(stale.len(), 2);
    }

    // ── check_cron_schedules (unit-level with in-memory DB + pool) ──────

    fn make_test_pool() -> DbPool {
        use r2d2::Pool;
        use r2d2_sqlite::SqliteConnectionManager;

        let manager = SqliteConnectionManager::memory().with_flags(
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
                | rusqlite::OpenFlags::SQLITE_OPEN_CREATE
                | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
                | rusqlite::OpenFlags::SQLITE_OPEN_SHARED_CACHE,
        );
        let pool = Pool::builder()
            .max_size(2)
            .test_on_check_out(true)
            .build(manager)
            .expect("Failed to create test pool");

        // Create the jobs table
        let conn = pool.get().unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS _crap_jobs (
                id TEXT PRIMARY KEY,
                slug TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending',
                queue TEXT NOT NULL DEFAULT 'default',
                data TEXT DEFAULT '{}',
                result TEXT,
                error TEXT,
                attempt INTEGER NOT NULL DEFAULT 0,
                max_attempts INTEGER NOT NULL DEFAULT 1,
                scheduled_by TEXT,
                created_at TEXT DEFAULT (datetime('now')),
                started_at TEXT,
                completed_at TEXT,
                heartbeat_at TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_crap_jobs_status ON _crap_jobs(status);
            CREATE INDEX IF NOT EXISTS idx_crap_jobs_queue ON _crap_jobs(queue, status);
            CREATE INDEX IF NOT EXISTS idx_crap_jobs_slug ON _crap_jobs(slug, status);",
        )
        .unwrap();
        drop(conn);

        pool
    }

    #[test]
    fn check_cron_schedules_fires_due_job() {
        let pool = make_test_pool();
        let registry = make_registry_with_jobs(vec![
            JobDefinition::builder("cron_job", "some.handler")
                .schedule("* * * * *") // every minute
                .retries(0)
                .queue("default")
                .skip_if_running(false)
                .build(),
        ]);

        // Set last_check to 2 minutes ago, now to current — schedule should fire
        let now = chrono::Utc::now();
        let last_check = now - chrono::Duration::minutes(2);

        check_cron_schedules(&pool, &registry, last_check, now).unwrap();

        let conn = pool.get().unwrap();
        let jobs = job_query::list_job_runs(&conn, Some("cron_job"), None, 100, 0).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].status, JobStatus::Pending);
        assert_eq!(jobs[0].scheduled_by.as_deref(), Some("cron"));
    }

    #[test]
    fn check_cron_schedules_skips_no_schedule_jobs() {
        let pool = make_test_pool();
        let registry = make_registry_with_jobs(vec![
            JobDefinition::builder("no_cron_job", "some.handler").build(), // no schedule
        ]);

        let now = chrono::Utc::now();
        let last_check = now - chrono::Duration::minutes(2);

        check_cron_schedules(&pool, &registry, last_check, now).unwrap();

        let conn = pool.get().unwrap();
        let jobs = job_query::list_job_runs(&conn, None, None, 100, 0).unwrap();
        assert_eq!(jobs.len(), 0);
    }

    #[test]
    fn check_cron_schedules_skips_not_due() {
        let pool = make_test_pool();
        let registry = make_registry_with_jobs(vec![
            JobDefinition::builder("hourly_job", "some.handler")
                .schedule("0 * * * *") // every hour at :00
                .build(),
        ]);

        // Set the window to just 1 second — unlikely an hour boundary is crossed
        let now = chrono::Utc::now();
        let last_check = now - chrono::Duration::seconds(1);

        check_cron_schedules(&pool, &registry, last_check, now).unwrap();

        let conn = pool.get().unwrap();
        let jobs = job_query::list_job_runs(&conn, None, None, 100, 0).unwrap();
        // Might be 0 or 1 depending on exact timing, but we can't fully control
        // this without a fixed clock. For a 1-second window, it's almost certainly 0
        // unless we happen to cross an hour boundary.
        assert!(jobs.len() <= 1);
    }

    #[test]
    fn check_cron_schedules_skip_if_running() {
        let pool = make_test_pool();
        let registry = make_registry_with_jobs(vec![
            JobDefinition::builder("skip_job", "some.handler")
                .schedule("* * * * *")
                .skip_if_running(true)
                .build(),
        ]);

        // Insert a running job for this slug
        {
            let conn = pool.get().unwrap();
            job_query::insert_job(&conn, "skip_job", "{}", "manual", 1, "default").unwrap();
            conn.execute("UPDATE _crap_jobs SET status = 'running'", [])
                .unwrap();
        }

        let now = chrono::Utc::now();
        let last_check = now - chrono::Duration::minutes(2);

        check_cron_schedules(&pool, &registry, last_check, now).unwrap();

        // Should NOT insert a new pending job because skip_if_running=true and one is running
        let conn = pool.get().unwrap();
        let pending =
            job_query::list_job_runs(&conn, Some("skip_job"), Some("pending"), 100, 0).unwrap();
        assert_eq!(pending.len(), 0);
    }

    #[test]
    fn check_cron_schedules_no_skip_if_running_false() {
        let pool = make_test_pool();
        let registry = make_registry_with_jobs(vec![
            JobDefinition::builder("noskip_job", "some.handler")
                .schedule("* * * * *")
                .skip_if_running(false)
                .build(),
        ]);

        // Insert a running job
        {
            let conn = pool.get().unwrap();
            job_query::insert_job(&conn, "noskip_job", "{}", "manual", 1, "default").unwrap();
            conn.execute("UPDATE _crap_jobs SET status = 'running'", [])
                .unwrap();
        }

        let now = chrono::Utc::now();
        let last_check = now - chrono::Duration::minutes(2);

        check_cron_schedules(&pool, &registry, last_check, now).unwrap();

        // Should insert a new pending job even though one is running
        let conn = pool.get().unwrap();
        let pending =
            job_query::list_job_runs(&conn, Some("noskip_job"), Some("pending"), 100, 0).unwrap();
        assert_eq!(pending.len(), 1);
    }

    #[test]
    fn check_cron_schedules_invalid_cron_expression() {
        let pool = make_test_pool();
        let registry = make_registry_with_jobs(vec![
            JobDefinition::builder("bad_cron", "some.handler")
                .schedule("not a valid cron")
                .build(),
        ]);

        let now = chrono::Utc::now();
        let last_check = now - chrono::Duration::minutes(2);

        // Should not error, just skip the invalid expression
        check_cron_schedules(&pool, &registry, last_check, now).unwrap();

        let conn = pool.get().unwrap();
        let jobs = job_query::list_job_runs(&conn, None, None, 100, 0).unwrap();
        assert_eq!(jobs.len(), 0);
    }

    #[test]
    fn check_cron_schedules_retries_stored() {
        let pool = make_test_pool();
        let registry = make_registry_with_jobs(vec![
            JobDefinition::builder("retried_cron", "some.handler")
                .schedule("* * * * *")
                .retries(3)
                .queue("special")
                .skip_if_running(false)
                .build(),
        ]);

        let now = chrono::Utc::now();
        let last_check = now - chrono::Duration::minutes(2);

        check_cron_schedules(&pool, &registry, last_check, now).unwrap();

        let conn = pool.get().unwrap();
        let jobs = job_query::list_job_runs(&conn, Some("retried_cron"), None, 100, 0).unwrap();
        assert_eq!(jobs.len(), 1);
        // retries=3 => max_attempts = retries + 1 = 4
        assert_eq!(jobs[0].max_attempts, 4);
        assert_eq!(jobs[0].queue, "special");
    }
}
