//! Background job scheduler: polls for pending jobs, evaluates cron schedules,
//! executes Lua handlers, and manages heartbeats and stale recovery.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use crate::config::JobsConfig;
use crate::core::SharedRegistry;
use crate::core::job::JobDefinition;
use crate::db::DbPool;
use crate::db::query::jobs as job_query;
use crate::hooks::lifecycle::HookRunner;

/// Start the scheduler background loop. Runs until the task is cancelled.
pub async fn start(
    pool: DbPool,
    hook_runner: HookRunner,
    registry: SharedRegistry,
    config: JobsConfig,
) -> Result<()> {
    tracing::info!("Scheduler started (poll={}s, cron={}s, max_concurrent={})",
        config.poll_interval, config.cron_interval, config.max_concurrent);

    // Recover stale jobs on startup
    {
        let conn = pool.get().context("Scheduler: failed to get DB connection for recovery")?;
        recover_stale_jobs(&conn, &registry)?;
    }

    let poll_interval = tokio::time::Duration::from_secs(config.poll_interval);
    let cron_interval = tokio::time::Duration::from_secs(config.cron_interval);
    let heartbeat_interval = tokio::time::Duration::from_secs(config.heartbeat_interval);
    let auto_purge_secs = config.auto_purge_seconds();

    let mut poll_ticker = tokio::time::interval(poll_interval);
    let mut cron_ticker = tokio::time::interval(cron_interval);
    let mut heartbeat_ticker = tokio::time::interval(heartbeat_interval);

    // Track running job IDs for heartbeat updates
    let running_jobs: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));

    // Track last cron check time to avoid duplicate firing
    let mut last_cron_check = chrono::Utc::now();

    // Auto-purge timer: check once per cron interval
    let mut purge_counter: u64 = 0;

    loop {
        tokio::select! {
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
                if purge_counter % 10 == 0 {
                    if let Some(secs) = auto_purge_secs {
                        if let Ok(conn) = pool.get() {
                            match job_query::purge_old_jobs(&conn, secs) {
                                Ok(n) if n > 0 => tracing::info!("Auto-purged {} old job run(s)", n),
                                Ok(_) => {}
                                Err(e) => tracing::warn!("Auto-purge error: {}", e),
                            }
                        }
                    }
                }
            }
            _ = heartbeat_ticker.tick() => {
                // Update heartbeats for all running jobs
                let ids: Vec<String> = running_jobs.lock()
                    .map(|guard| guard.clone())
                    .unwrap_or_default();

                if !ids.is_empty() {
                    if let Ok(conn) = pool.get() {
                        for id in &ids {
                            if let Err(e) = job_query::update_heartbeat(&conn, id) {
                                tracing::warn!("Heartbeat update error for {}: {}", id, e);
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Poll for pending jobs and execute them.
async fn poll_and_execute(
    pool: &DbPool,
    hook_runner: &HookRunner,
    registry: &SharedRegistry,
    max_concurrent: usize,
    running_jobs: &Arc<std::sync::Mutex<Vec<String>>>,
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
        let reg = registry.read()
            .map_err(|e| anyhow::anyhow!("Registry lock poisoned: {}", e))?;
        reg.jobs.iter()
            .map(|(slug, def)| (slug.clone(), def.concurrency))
            .collect::<HashMap<String, u32>>()
    };

    let claimed = job_query::claim_pending_jobs(&conn, available, &running_counts, &job_concurrency)?;
    drop(conn);

    for job_run in claimed {
        let job_def = {
            let reg = registry.read()
                .map_err(|e| anyhow::anyhow!("Registry lock poisoned: {}", e))?;
            match reg.get_job(&job_run.slug) {
                Some(def) => def.clone(),
                None => {
                    tracing::warn!("Job definition '{}' not found, marking as failed", job_run.slug);
                    if let Ok(c) = pool.get() {
                        let _ = job_query::fail_job(&c, &job_run.id, "job definition not found", false);
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

        // Execute the job in a blocking task (same pattern as hook execution)
        tokio::task::spawn_blocking(move || {
            let result = execute_job(&pool, &hook_runner, &job_def, &job_run);

            // Remove from running tracking
            if let Ok(mut guard) = running_jobs.lock() {
                guard.retain(|id| id != &job_id);
            }

            if let Err(e) = result {
                tracing::error!("Job {} ({}) execution error: {}", job_id, job_run.slug, e);
            }
        });
    }

    Ok(())
}

/// Execute a single job: call the Lua handler with CRUD access.
fn execute_job(
    pool: &DbPool,
    hook_runner: &HookRunner,
    job_def: &JobDefinition,
    job_run: &crate::core::job::JobRun,
) -> Result<()> {
    let timeout = std::time::Duration::from_secs(job_def.timeout);
    let start = std::time::Instant::now();

    tracing::info!("Executing job {} ({}) attempt {}/{}",
        job_run.id, job_run.slug, job_run.attempt, job_run.max_attempts);

    // Open a transaction for the job handler (same TxContext pattern as hooks)
    let mut conn = pool.get().context("Failed to get DB connection for job")?;
    let tx = conn.transaction().context("Failed to begin job transaction")?;

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
            let c = pool.get().context("Failed to get DB connection for completion")?;
            job_query::complete_job(&c, &job_run.id, result_json.as_deref())?;
            let elapsed = start.elapsed();
            tracing::info!("Job {} ({}) completed in {:?}", job_run.id, job_run.slug, elapsed);
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
            let c = pool.get().context("Failed to get DB connection for failure")?;
            job_query::fail_job(&c, &job_run.id, &error_msg, should_retry)?;
            if should_retry {
                tracing::warn!("Job {} ({}) failed (attempt {}/{}), will retry: {}",
                    job_run.id, job_run.slug, job_run.attempt, job_run.max_attempts, error_msg);
            } else {
                tracing::error!("Job {} ({}) failed permanently: {}",
                    job_run.id, job_run.slug, error_msg);
            }
        }
    }

    Ok(())
}

/// Check cron schedules and insert pending jobs for due ones.
fn check_cron_schedules(
    pool: &DbPool,
    registry: &SharedRegistry,
    last_check: chrono::DateTime<chrono::Utc>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    let reg = registry.read()
        .map_err(|e| anyhow::anyhow!("Registry lock poisoned: {}", e))?;

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
                tracing::warn!("Invalid cron expression '{}' for job '{}': {}", schedule_str, slug, e);
                continue;
            }
        };

        // Check if the schedule should have fired between last_check and now
        let should_fire = schedule.after(&last_check)
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
        let job = job_query::insert_job(
            &conn,
            slug,
            "{}",
            "cron",
            def.retries + 1,
            &def.queue,
        )?;
        tracing::info!("Cron scheduled job '{}' (run {})", slug, job.id);
    }

    Ok(())
}

/// Recover stale jobs on startup.
fn recover_stale_jobs(
    conn: &rusqlite::Connection,
    registry: &SharedRegistry,
) -> Result<()> {
    let reg = registry.read()
        .map_err(|e| anyhow::anyhow!("Registry lock poisoned: {}", e))?;

    // Find all running jobs — on startup, these are stale (server was restarted)
    let stale = job_query::find_stale_jobs(conn, 0)?;

    for job in &stale {
        let timeout = reg.jobs.get(&job.slug)
            .map(|d| d.timeout)
            .unwrap_or(60);
        let threshold = std::cmp::max(timeout * 2, 300);

        // Any job that was running when we started is stale
        let error = format!("stale: server restarted (was running, timeout={}s)", threshold);
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
}
