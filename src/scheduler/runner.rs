//! Job execution, cron scheduling, stale recovery, cron normalization, and soft-delete purge.

use std::{str::FromStr, time::Instant};

use anyhow::{Context as _, Result, anyhow};
use chrono::{DateTime, Utc};
use cron::Schedule;
use serde_json::from_str;
use tracing::{debug, error, info, warn};

use crate::{
    config::LocaleConfig,
    core::{
        CollectionDefinition, SharedRegistry,
        email::{EmailJobData, EmailProvider, SYSTEM_EMAIL_JOB},
        job::{JobDefinition, JobRun},
        upload,
        upload::StorageBackend,
    },
    db::{DbConnection, DbPool, DbValue, query, query::jobs as job_query},
    hooks::HookRunner,
};

/// Execute a single job: call the Lua handler with CRUD access,
/// or handle system jobs (like `_system_email`) directly in Rust.
pub fn execute_job(
    pool: &DbPool,
    hook_runner: &HookRunner,
    job_def: &JobDefinition,
    job_run: &JobRun,
    email_provider: Option<&dyn EmailProvider>,
) -> Result<()> {
    let start = Instant::now();

    info!(
        "Executing job {} ({}) attempt {}/{}",
        job_run.id, job_run.slug, job_run.attempt, job_run.max_attempts
    );

    // System email job: handle directly without Lua VM
    if job_run.slug == SYSTEM_EMAIL_JOB {
        return execute_system_email(pool, job_run, email_provider, start);
    }

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

            info!(
                "Job {} ({}) completed in {:?}",
                job_run.id, job_run.slug, elapsed
            );
        }
        Err(e) => {
            // Explicit drop triggers rollback (BoxedTransaction rolls back on drop)
            drop(tx);
            let error_msg = e.to_string();
            let should_retry = job_run.attempt < job_run.max_attempts;
            let c = pool
                .get()
                .context("Failed to get DB connection for failure")?;

            job_query::fail_job(&c, &job_run.id, &error_msg, should_retry, job_run.attempt)?;

            if should_retry {
                warn!(
                    "Job {} ({}) failed (attempt {}/{}), will retry: {}",
                    job_run.id, job_run.slug, job_run.attempt, job_run.max_attempts, error_msg
                );
            } else {
                error!(
                    "Job {} ({}) failed permanently: {}",
                    job_run.id, job_run.slug, error_msg
                );
            }
        }
    }

    Ok(())
}

/// Execute a `_system_email` job: parse data and send via email provider.
fn execute_system_email(
    pool: &DbPool,
    job_run: &JobRun,
    email_provider: Option<&dyn EmailProvider>,
    start: Instant,
) -> Result<()> {
    let provider = email_provider
        .ok_or_else(|| anyhow!("System email job requires email provider but none configured"))?;

    let data: EmailJobData = from_str(&job_run.data).context("Invalid email job data")?;

    let result = provider.send(&data.to, &data.subject, &data.html, data.text.as_deref());

    match result {
        Ok(()) => {
            let c = pool
                .get()
                .context("Failed to get DB connection for email job completion")?;

            job_query::complete_job(&c, &job_run.id, None)?;

            let elapsed = start.elapsed();

            info!(
                "Email job {} completed in {:?} (to: {})",
                job_run.id, elapsed, data.to
            );
        }
        Err(e) => {
            let error_msg = format!("{:#}", e);
            let should_retry = job_run.attempt < job_run.max_attempts;
            let c = pool
                .get()
                .context("Failed to get DB connection for email job failure")?;

            job_query::fail_job(&c, &job_run.id, &error_msg, should_retry, job_run.attempt)?;

            if should_retry {
                warn!(
                    "Email job {} failed (attempt {}/{}), will retry: {}",
                    job_run.id, job_run.attempt, job_run.max_attempts, error_msg
                );
            } else {
                error!("Email job {} failed permanently: {}", job_run.id, error_msg);
            }
        }
    }

    Ok(())
}

/// Check cron schedules and insert pending jobs for due ones.
pub fn check_cron_schedules(
    pool: &DbPool,
    registry: &SharedRegistry,
    last_check: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<()> {
    let reg = registry
        .read()
        .map_err(|e| anyhow!("Registry lock poisoned: {}", e))?;

    let mut conn = pool.get().context("Failed to get DB connection for cron")?;
    let tx = conn
        .transaction_immediate()
        .context("Failed to start cron check transaction")?;

    for (slug, def) in &reg.jobs {
        let schedule_str = match &def.schedule {
            Some(s) => s,
            None => continue,
        };

        // Parse cron expression (the cron crate expects 6-7 fields with seconds;
        // normalize standard 5-field expressions by prepending "0" for seconds)
        let normalized = normalize_cron(schedule_str);
        let schedule = match Schedule::from_str(&normalized) {
            Ok(s) => s,
            Err(e) => {
                warn!(
                    "Invalid cron expression '{}' for job '{}': {}",
                    schedule_str, slug, e
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

        // Atomic cron dedup: only one instance wins each cron window.
        // Uses _crap_cron_fired table to prevent double-fire in multi-server.
        let fired_at = now.to_rfc3339();
        let window_start = last_check.to_rfc3339();

        if !job_query::try_claim_cron_window(&tx, slug, &fired_at, &window_start)? {
            debug!(
                "Cron job '{}' already fired by another instance in this window",
                slug
            );

            continue;
        }

        // Check skip_if_running (atomic with insert inside the same IMMEDIATE transaction)
        if def.skip_if_running {
            let running = job_query::count_running(&tx, Some(slug))?;

            if running > 0 {
                debug!("Skipping cron job '{}' — still running", slug);

                continue;
            }
        }

        // Insert a pending job
        let job = job_query::insert_job(&tx, slug, "{}", "cron", def.retries + 1, &def.queue)?;

        info!("Cron scheduled job '{}' (run {})", slug, job.id);
    }

    tx.commit()
        .context("Failed to commit cron check transaction")?;

    Ok(())
}

/// Recover stale jobs on startup.
pub fn recover_stale_jobs(conn: &dyn DbConnection, registry: &SharedRegistry) -> Result<()> {
    let reg = registry
        .read()
        .map_err(|e| anyhow!("Registry lock poisoned: {}", e))?;

    // Find all running jobs — on startup, these are stale (server was restarted)
    let stale = job_query::find_stale_jobs(conn, 0)?;

    for job in &stale {
        let timeout = reg
            .jobs
            .get(job.slug.as_str())
            .map(|d| d.timeout)
            .unwrap_or(60);

        // Any job that was running when we started is stale
        let error = format!(
            "stale: server restarted (was running, timeout={}s)",
            timeout
        );
        job_query::mark_stale(conn, &job.id, &error)?;
        info!("Marked stale job {} ({})", job.id, job.slug);
    }

    if !stale.is_empty() {
        info!("Recovered {} stale job(s)", stale.len());
    }

    Ok(())
}

/// Parse a retention duration string like "30d", "7d", "24h" into seconds.
/// Returns `None` if the string is not a valid duration.
pub(crate) fn parse_retention_seconds(s: &str) -> Option<i64> {
    let s = s.trim();

    if let Some(days) = s.strip_suffix('d') {
        days.parse::<i64>().ok().map(|d| d * 86400)
    } else if let Some(hours) = s.strip_suffix('h') {
        hours.parse::<i64>().ok().map(|h| h * 3600)
    } else if let Some(mins) = s.strip_suffix('m') {
        mins.parse::<i64>().ok().map(|m| m * 60)
    } else if let Some(secs) = s.strip_suffix('s') {
        secs.parse::<i64>().ok()
    } else {
        s.parse::<i64>().ok() // raw seconds
    }
}

/// Purge soft-deleted documents past their retention period.
///
/// For each collection with `soft_delete` + `soft_delete_retention`, find docs
/// where `_deleted_at` is older than the retention threshold and hard-delete them.
/// Upload files are cleaned up before deletion.
pub fn purge_soft_deleted(
    conn: &dyn DbConnection,
    registry: &SharedRegistry,
    storage: &dyn StorageBackend,
    locale_config: &LocaleConfig,
) -> Result<u64> {
    let reg = registry
        .read()
        .map_err(|e| anyhow!("Registry lock poisoned: {}", e))?;

    let mut total = 0u64;

    for (slug, def) in &reg.collections {
        if !def.soft_delete {
            continue;
        }

        let Some(ref retention) = def.soft_delete_retention else {
            continue;
        };

        let Some(seconds) = parse_retention_seconds(retention) else {
            warn!(
                "Invalid soft_delete_retention '{}' for collection '{}'",
                retention, slug
            );
            continue;
        };

        let purged = purge_collection(conn, slug, def, seconds, storage, locale_config)?;
        total += purged;
    }

    Ok(total)
}

/// Purge expired soft-deleted documents from a single collection.
///
/// Collects upload file data before deleting from DB, then removes files
/// from disk after the DB deletes succeed. A crash between DB delete and
/// file delete leaves orphaned files (safe), rather than orphaned DB records
/// pointing to deleted files (unsafe).
fn purge_collection(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    retention_seconds: i64,
    storage: &dyn StorageBackend,
    locale_config: &LocaleConfig,
) -> Result<u64> {
    // Find docs past the retention threshold
    let (offset_sql, offset_param) = conn.date_offset_expr(retention_seconds, 1);
    let threshold_sql = format!(
        "SELECT id FROM \"{}\" WHERE _deleted_at IS NOT NULL \
         AND _deleted_at < {}",
        slug, offset_sql
    );
    let rows = conn.query_all(&threshold_sql, &[offset_param])?;

    let mut purged = 0u64;
    let mut upload_docs = Vec::new();

    for row in &rows {
        let id = match row.get_value(0) {
            Some(DbValue::Text(s)) => s.clone(),
            _ => continue,
        };

        // Skip documents that are still referenced — protect referential integrity.
        // Uses locked variant to prevent concurrent creates from incrementing ref count
        // between this check and the DELETE (Postgres only; SQLite serializes via IMMEDIATE).
        let ref_count = query::ref_count::get_ref_count_locked(conn, slug, &id)?.unwrap_or(0);
        if ref_count > 0 {
            debug!(
                "Skipping purge of {}/{}: referenced by {} document(s)",
                slug, id, ref_count
            );
            continue;
        }

        // Decrement ref counts on targets before hard delete (CASCADE removes junction rows)
        query::ref_count::before_hard_delete(conn, slug, &id, &def.fields, locale_config)?;

        // Collect upload file paths BEFORE deleting from DB
        if def.is_upload_collection()
            && let Ok(Some(doc)) = query::find_by_id_unfiltered(conn, slug, def, &id, None)
        {
            upload_docs.push(doc);
        }

        // Cancel pending image conversions
        if def.is_upload_collection() {
            let _ = query::images::delete_entries_for_document(conn, slug, &id);
        }

        // Clean up FTS index before hard delete
        if conn.supports_fts() {
            query::fts::fts_delete(conn, slug, &id)?;
        }

        // Hard delete the document from DB
        query::delete(conn, slug, &id)?;
        purged += 1;
    }

    // Delete files AFTER all DB deletes have succeeded.
    // If the process crashes here, we get orphaned files (harmless)
    // rather than DB records pointing to missing files (harmful).
    for doc in &upload_docs {
        upload::delete_upload_files(storage, &doc.fields);
    }

    if purged > 0 {
        info!(
            "Purged {} expired soft-deleted doc(s) from '{}'",
            purged, slug
        );
    }

    Ok(purged)
}

/// Dedup slug used to claim the retention-purge cron tick via
/// `_crap_cron_fired`. Retention purge is a "pseudo cron" job — it runs on a
/// fixed interval from the scheduler loop rather than a user-defined cron
/// expression, but must still be deduped across instances in multi-node
/// deployments.
pub const RETENTION_PURGE_SLUG: &str = "__retention_purge";

/// Attempt to claim the retention-purge tick for this instance/window.
///
/// Returns `true` iff this caller won the tick and should run the purge.
/// Uses the same `_crap_cron_fired` dedup table as user cron jobs.
/// `window_seconds` must match the scheduler's purge cadence so two instances
/// firing inside the same window still end up with exactly one winner.
pub fn claim_retention_purge_tick(
    conn: &dyn DbConnection,
    now: DateTime<Utc>,
    window_seconds: i64,
) -> Result<bool> {
    let fired_at = now.to_rfc3339();
    let window_start = (now - chrono::Duration::seconds(window_seconds)).to_rfc3339();

    job_query::try_claim_cron_window(conn, RETENTION_PURGE_SLUG, &fired_at, &window_start)
}

/// Normalize a cron expression: the `cron` crate expects 6 or 7 fields (with a
/// leading seconds field), but users write standard 5-field cron (`0 3 * * *`).
/// If the expression has exactly 5 fields, prepend "0" for seconds.
pub(crate) fn normalize_cron(expr: &str) -> String {
    let fields: Vec<&str> = expr.split_whitespace().collect();

    if fields.len() == 5 {
        format!("0 {}", fields.join(" "))
    } else {
        fields.join(" ")
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use chrono::Timelike;
    use r2d2::Pool;
    use r2d2_sqlite::SqliteConnectionManager;

    use super::*;
    use crate::core::{Registry, job::JobStatus};

    // ── parse_retention_seconds ───────────────────────────────────────────

    #[test]
    fn parse_retention_days() {
        assert_eq!(parse_retention_seconds("30d"), Some(30 * 86400));
        assert_eq!(parse_retention_seconds("7d"), Some(7 * 86400));
        assert_eq!(parse_retention_seconds("1d"), Some(86400));
    }

    #[test]
    fn parse_retention_hours() {
        assert_eq!(parse_retention_seconds("24h"), Some(24 * 3600));
        assert_eq!(parse_retention_seconds("1h"), Some(3600));
    }

    #[test]
    fn parse_retention_minutes() {
        assert_eq!(parse_retention_seconds("30m"), Some(1800));
        assert_eq!(parse_retention_seconds("1m"), Some(60));
    }

    #[test]
    fn parse_retention_seconds_suffix() {
        assert_eq!(parse_retention_seconds("10s"), Some(10));
        assert_eq!(parse_retention_seconds("1s"), Some(1));
        assert_eq!(parse_retention_seconds("0s"), Some(0));
    }

    #[test]
    fn parse_retention_raw_seconds() {
        assert_eq!(parse_retention_seconds("3600"), Some(3600));
        assert_eq!(parse_retention_seconds("86400"), Some(86400));
    }

    #[test]
    fn parse_retention_invalid() {
        assert_eq!(parse_retention_seconds("abc"), None);
        assert_eq!(parse_retention_seconds(""), None);
        assert_eq!(parse_retention_seconds("d"), None);
    }

    #[test]
    fn parse_retention_with_whitespace() {
        assert_eq!(parse_retention_seconds(" 30d "), Some(30 * 86400));
        assert_eq!(parse_retention_seconds(" 3600 "), Some(3600));
    }

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
        // split_whitespace handles multiple spaces — normalizes to single spaces
        let result = normalize_cron("0  3  *  *  *");
        assert_eq!(result, "0 0 3 * * *");
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
        let pool = make_test_pool();
        let conn = pool.get().unwrap();
        let registry = make_registry_with_jobs(vec![
            JobDefinition::builder("my_job", "some.handler")
                .timeout(120)
                .build(),
        ]);

        // Insert a running job (simulates server crash with running job)
        job_query::insert_job(&conn, "my_job", "{}", "manual", 1, "default").unwrap();
        conn.execute_batch(
            "UPDATE _crap_jobs SET status = 'running', heartbeat_at = datetime('now', '-600 seconds')",
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
        assert!(jobs[0].error.as_ref().unwrap().contains("timeout=120s"));
    }

    #[test]
    fn recover_stale_jobs_uses_job_timeout() {
        let pool = make_test_pool();
        let conn = pool.get().unwrap();
        let registry = make_registry_with_jobs(vec![
            JobDefinition::builder("long_job", "some.handler")
                .timeout(3600)
                .build(), // 1 hour
        ]);

        job_query::insert_job(&conn, "long_job", "{}", "manual", 1, "default").unwrap();
        conn.execute_batch(
            "UPDATE _crap_jobs SET status = 'running', heartbeat_at = datetime('now', '-600 seconds')",
        ).unwrap();

        recover_stale_jobs(&conn, &registry).unwrap();

        let jobs = job_query::list_job_runs(&conn, None, Some("stale"), 100, 0).unwrap();
        assert_eq!(jobs.len(), 1);
        assert!(jobs[0].error.as_ref().unwrap().contains("timeout=3600s"));
    }

    #[test]
    fn recover_stale_jobs_default_timeout_for_unknown_slug() {
        let pool = make_test_pool();
        let conn = pool.get().unwrap();
        // Registry has no job definitions — slug not found, uses default timeout=60
        let registry = make_registry_with_jobs(vec![]);

        job_query::insert_job(&conn, "unknown_job", "{}", "manual", 1, "default").unwrap();
        conn.execute_batch(
            "UPDATE _crap_jobs SET status = 'running', heartbeat_at = datetime('now', '-600 seconds')",
        ).unwrap();

        recover_stale_jobs(&conn, &registry).unwrap();

        let jobs = job_query::list_job_runs(&conn, None, Some("stale"), 100, 0).unwrap();
        assert_eq!(jobs.len(), 1);
        assert!(jobs[0].error.as_ref().unwrap().contains("timeout=60s"));
    }

    #[test]
    fn recover_stale_jobs_no_running_is_noop() {
        let pool = make_test_pool();
        let conn = pool.get().unwrap();
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
        let pool = make_test_pool();
        let conn = pool.get().unwrap();
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
        conn.execute_batch("UPDATE _crap_jobs SET status = 'running'")
            .unwrap();

        recover_stale_jobs(&conn, &registry).unwrap();

        let stale = job_query::list_job_runs(&conn, None, Some("stale"), 100, 0).unwrap();
        assert_eq!(stale.len(), 2);
    }

    // ── check_cron_schedules (unit-level with in-memory DB + pool) ──────

    fn make_test_pool() -> DbPool {
        let manager = SqliteConnectionManager::memory().with_flags(
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
                | rusqlite::OpenFlags::SQLITE_OPEN_CREATE
                | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
                | rusqlite::OpenFlags::SQLITE_OPEN_SHARED_CACHE,
        );
        let inner = Pool::builder()
            .max_size(2)
            .test_on_check_out(true)
            .build(manager)
            .expect("Failed to create test pool");

        let pool = DbPool::from_pool(inner);

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
                heartbeat_at TEXT,
                retry_after TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_crap_jobs_status ON _crap_jobs(status);
            CREATE INDEX IF NOT EXISTS idx_crap_jobs_queue ON _crap_jobs(queue, status);
            CREATE INDEX IF NOT EXISTS idx_crap_jobs_slug ON _crap_jobs(slug, status);
            CREATE TABLE IF NOT EXISTS _crap_cron_fired (
                slug TEXT PRIMARY KEY,
                fired_at TEXT NOT NULL DEFAULT (datetime('now'))
            );",
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

        // Use a fixed window that is guaranteed to NOT cross an hour boundary:
        // pick a time at minute :30 with a 1-second window.
        let now = chrono::Utc::now()
            .with_minute(30)
            .unwrap()
            .with_second(30)
            .unwrap();
        let last_check = now - chrono::Duration::seconds(1);

        check_cron_schedules(&pool, &registry, last_check, now).unwrap();

        let conn = pool.get().unwrap();
        let jobs = job_query::list_job_runs(&conn, None, None, 100, 0).unwrap();
        assert_eq!(
            jobs.len(),
            0,
            "hourly job should not fire in a 1s window at :30"
        );
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
            conn.execute_batch("UPDATE _crap_jobs SET status = 'running'")
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
            conn.execute_batch("UPDATE _crap_jobs SET status = 'running'")
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

    /// Two concurrent claims in the same window: only one wins. Locks in the
    /// retention-purge dedup so multi-node deployments don't run the purge N
    /// times per tick.
    #[test]
    fn retention_purge_claims_cron_tick_atomically() {
        let pool = make_test_pool();
        let conn = pool.get().unwrap();

        let now = chrono::Utc::now();
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
        let later = now + chrono::Duration::seconds(window_secs * 2);
        let third = claim_retention_purge_tick(&conn, later, window_secs)
            .expect("later claim must succeed");
        assert!(third, "a claim past the window should win again");
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
