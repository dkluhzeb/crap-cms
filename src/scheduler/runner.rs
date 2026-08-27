//! Job execution, cron scheduling, stale recovery, cron normalization, and soft-delete purge.

use std::{collections::HashMap, str::FromStr, time::Instant};

use anyhow::{Context as _, Result, anyhow};
use chrono::{DateTime, Utc};
use cron::Schedule;
use serde_json::from_str;
use tracing::{debug, error, info, warn};

use crate::{
    config::LocaleConfig,
    core::{
        CollectionDefinition, DocumentFields, JobDefinition, JobRun, Registry,
        email::{EmailJobData, EmailProvider, SYSTEM_EMAIL_JOB},
        upload::{self, ImageConvertJobData, SYSTEM_IMAGE_CONVERT_JOB, SharedStorage},
    },
    db::{DbConnection, DbPool, DbValue, query, query::jobs as job_query},
    hooks::{HookRunner, LuaCrudInfra},
};

/// Write a job-failure outcome to the queue row and log it at the right level.
///
/// `label` is the human job identifier for log lines (e.g. `"Job abc (slug)"`);
/// `error_msg` is the already-rendered failure text stored on the row. The
/// single place the failure write + retry/permanent log-level split lives, so
/// every job kind records failures identically.
fn write_job_failure(
    pool: &DbPool,
    job_run: &JobRun,
    label: &str,
    error_msg: &str,
    should_retry: bool,
) -> Result<()> {
    let c = pool
        .get()
        .context("Failed to get DB connection to record job failure")?;

    job_query::fail_job(&c, &job_run.id, error_msg, should_retry, job_run.attempt)?;

    if should_retry {
        warn!(
            "{label} failed (attempt {}/{}), will retry: {error_msg}",
            job_run.attempt, job_run.max_attempts
        );
    } else {
        error!("{label} failed permanently: {error_msg}");
    }

    Ok(())
}

/// Record a retryable job failure: render the error with its full anyhow cause
/// chain (`{:#}` — in ONE place, so no job kind silently drops diagnostic detail
/// the way the user-job path used to with `to_string()`) and honor the attempt
/// budget. Shared by the Lua-handler, system-email, and image-convert paths.
fn record_job_failure(
    pool: &DbPool,
    job_run: &JobRun,
    label: &str,
    err: &anyhow::Error,
) -> Result<()> {
    let should_retry = job_run.attempt < job_run.max_attempts;
    write_job_failure(pool, job_run, label, &format!("{err:#}"), should_retry)
}

/// Record a permanent (never-retried) job failure — e.g. a malformed system-job
/// payload that no retry can fix.
fn record_permanent_job_failure(
    pool: &DbPool,
    job_run: &JobRun,
    label: &str,
    error_msg: &str,
) -> Result<()> {
    write_job_failure(pool, job_run, label, error_msg, false)
}

/// Borrowed inputs for [`execute_job`], grouped per the >4-params rule.
/// All fields are references (or `Copy` options of references), so the
/// struct itself is `Copy` and passing it by value is free.
#[derive(Clone, Copy)]
pub struct ExecuteJobParams<'a> {
    pub pool: &'a DbPool,
    pub hook_runner: &'a HookRunner,
    pub job_def: &'a JobDefinition,
    pub job_run: &'a JobRun,
    pub email_provider: Option<&'a dyn EmailProvider>,
    pub storage: &'a SharedStorage,
    /// Event transport + populate cache for the handler's Lua CRUD calls
    /// (cloned per handler invocation; the queues stay `None` in pool-mode).
    /// `None` = job writes publish no events and skip cache invalidation.
    pub lua_infra: Option<&'a LuaCrudInfra>,
}

/// Execute a single job: call the Lua handler with CRUD access,
/// or handle system jobs (`_system_email`, `_system_image_convert`)
/// directly in Rust.
///
/// # Errors
///
/// Returns an error if the connection acquisition, Lua hook execution,
/// system-job handler, or job-status update fails.
pub fn execute_job(p: ExecuteJobParams<'_>) -> Result<()> {
    let ExecuteJobParams {
        pool,
        hook_runner,
        job_def,
        job_run,
        email_provider,
        storage,
        lua_infra,
    } = p;

    let start = Instant::now();

    info!(
        "Executing job {} ({}) attempt {}/{}",
        job_run.id, job_run.slug, job_run.attempt, job_run.max_attempts
    );

    // System email job: handle directly without Lua VM
    if job_run.slug == SYSTEM_EMAIL_JOB {
        return execute_system_email(pool, job_run, email_provider, start);
    }

    // System image-convert job: encode + write URL column + complete.
    // Rust handler — no Lua VM needed.
    if job_run.slug == SYSTEM_IMAGE_CONVERT_JOB {
        return execute_system_image_convert(pool, job_run, storage, start);
    }

    // Lua job handler runs in **pool-mode**: no outer transaction.
    // Each CRUD operation inside the handler opens its own short-lived
    // IMMEDIATE transaction (via `with_lua_db` / the `auto_tx` attribute
    // on every `#[lua_fn]` CRUD declaration). For multi-step atomicity
    // the user wraps a block in `crap.transaction(function() ... end)`,
    // which temporarily swaps the pool context for a shared tx context.
    // This avoids the `SQLITE_BUSY_SNAPSHOT` hazard that the previous
    // single-deferred-outer-tx model exposed for long-running handlers
    // that did read-then-write.
    let result = hook_runner.run_job_handler(&job_def.handler, job_run, pool, lua_infra.cloned());

    match result {
        Ok(result_json) => {
            let c = pool
                .get()
                .context("Failed to get DB connection for completion")?;

            job_query::complete_job(&c, &job_run.id, job_run.attempt, result_json.as_deref())?;

            let elapsed = start.elapsed();

            info!(
                "Job {} ({}) completed in {:?}",
                job_run.id, job_run.slug, elapsed
            );
        }
        Err(e) => {
            record_job_failure(
                pool,
                job_run,
                &format!("Job {} ({})", job_run.id, job_run.slug),
                &e,
            )?;
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

            job_query::complete_job(&c, &job_run.id, job_run.attempt, None)?;

            let elapsed = start.elapsed();

            info!(
                "Email job {} completed in {:?} (to: {})",
                job_run.id, elapsed, data.to
            );
        }
        Err(e) => {
            record_job_failure(pool, job_run, &format!("Email job {}", job_run.id), &e)?;
        }
    }

    Ok(())
}

/// Execute a `_system_image_convert` job: encode the source image,
/// write the converted bytes to storage, update the target document's
/// URL column, and mark the job completed. On encode / storage / DB
/// failure, defer to the job runner's standard `fail_job` retry path.
///
/// Mirrors the shape of [`execute_system_email`] — no Lua VM, no
/// outer transaction held during the slow encode step.
fn execute_system_image_convert(
    pool: &DbPool,
    job_run: &JobRun,
    storage: &SharedStorage,
    start: Instant,
) -> Result<()> {
    let data: ImageConvertJobData =
        from_str(&job_run.data).context("Invalid image-convert job data")?;

    let label = format!("Image-convert job {}", job_run.id);

    if !query::is_valid_identifier(&data.collection) {
        let error_msg = format!("invalid collection slug: {}", data.collection);
        record_permanent_job_failure(pool, job_run, &label, &error_msg)?;
        return Ok(());
    }
    if !query::is_valid_identifier(&data.url_column) {
        let error_msg = format!("invalid url_column: {}", data.url_column);
        record_permanent_job_failure(pool, job_run, &label, &error_msg)?;
        return Ok(());
    }

    let encode_result = upload::process_image_entry_with_storage(
        &data.source_path,
        &data.target_path,
        &data.format,
        data.quality,
        &**storage,
    );

    match encode_result {
        Ok(()) => {
            let mut conn = pool
                .get()
                .context("Failed to get DB connection for image-convert completion")?;

            // One IMMEDIATE tx wraps the URL write + completion mark so the
            // queue row never lands in `completed` while the document's URL
            // column is unchanged, or vice versa. Same atomicity property
            // the legacy `record_conversion_success` provided.
            let tx = conn
                .transaction_immediate()
                .context("Failed to begin image-convert completion transaction")?;

            tx.execute(
                &format!(
                    "UPDATE \"{}\" SET \"{}\" = {} WHERE id = {}",
                    data.collection,
                    data.url_column,
                    tx.placeholder(1),
                    tx.placeholder(2)
                ),
                &[
                    DbValue::Text(data.url_value.clone()),
                    DbValue::Text(data.document_id.clone()),
                ],
            )
            .context("Failed to update document URL column")?;

            job_query::complete_job(&tx, &job_run.id, job_run.attempt, None)?;

            tx.commit()
                .context("Failed to commit image-convert completion transaction")?;

            info!(
                "Image-convert job {} completed in {:?} ({} → {})",
                job_run.id,
                start.elapsed(),
                data.format,
                data.target_path
            );
        }
        Err(e) => {
            record_job_failure(pool, job_run, &label, &e)?;
        }
    }

    Ok(())
}

/// Check cron schedules and insert pending jobs for due ones.
///
/// # Errors
///
/// Returns an error if the connection, transaction, or job insertion fails.
pub fn check_cron_schedules(
    pool: &DbPool,
    registry: &Registry,
    last_check: DateTime<Utc>,
    now: DateTime<Utc>,
    queue_retries: &HashMap<String, u32>,
) -> Result<()> {
    let mut conn = pool.get().context("Failed to get DB connection for cron")?;
    let tx = conn
        .transaction_immediate()
        .context("Failed to start cron check transaction")?;

    for (slug, def) in &registry.jobs {
        let Some(schedule_str) = &def.schedule else {
            continue;
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

        // Insert a pending job. `effective_max_attempts` resolves
        // `JobDefinition.retries` first, falling back to
        // `[jobs.queues.<queue>] retries` when the definition didn't
        // set it.
        let job = job_query::insert_job(
            &tx,
            slug,
            "{}",
            "cron",
            def.effective_max_attempts(queue_retries.get(&def.queue).copied()),
            &def.queue,
            def.priority,
        )?;

        info!("Cron scheduled job '{}' (run {})", slug, job.id);
    }

    tx.commit()
        .context("Failed to commit cron check transaction")?;

    Ok(())
}

/// Recover stale jobs on startup.
///
/// # Errors
///
/// Returns an error if listing stale jobs or marking any one stale fails.
/// Recover jobs whose owning worker died mid-execution (heartbeat expired).
///
/// A `running` row whose `heartbeat_at` is older than `stale_threshold_secs`
/// (or null) is assumed dead — its worker stopped heartbeating. This is the
/// at-least-once delivery guarantee: a retryable dead job is **requeued** (so a
/// surviving peer re-runs it) and an exhausted one is marked terminal `stale`.
///
/// Runs both at startup (recover this node's own pre-crash jobs) and
/// periodically at runtime (any node reclaims a crashed peer's jobs). The
/// threshold MUST exceed the heartbeat interval so a merely-slow heartbeat
/// doesn't wrongly reclaim a live job; the caller passes
/// `heartbeat_interval * N`. All writes are compare-and-set on
/// `(running, attempt)` so two nodes recovering the same job — or the original
/// worker briefly resuming — cannot double-act or clobber the result.
pub fn recover_stale_jobs(
    conn: &dyn DbConnection,
    registry: &Registry,
    stale_threshold_secs: u64,
) -> Result<()> {
    let stale = job_query::find_stale_jobs(conn, stale_threshold_secs)?;

    let mut requeued = 0u32;
    let mut terminal = 0u32;

    for job in &stale {
        let _ = registry.jobs.get(job.slug.as_str()); // slug may be undefined; still recover

        if job.attempt < job.max_attempts {
            // Retryable → requeue with backoff (guarded on running+attempt).
            job_query::fail_job(
                conn,
                &job.id,
                "stale: worker heartbeat expired, requeued",
                true,
                job.attempt,
            )?;
            requeued += 1;
            info!(
                "Requeued stale job {} ({}) attempt {}/{}",
                job.id, job.slug, job.attempt, job.max_attempts
            );
        } else {
            // Retries exhausted → terminal stale (guarded).
            job_query::mark_stale(
                conn,
                &job.id,
                job.attempt,
                "stale: worker heartbeat expired, retries exhausted",
            )?;
            terminal += 1;
            info!("Marked stale job {} ({})", job.id, job.slug);
        }
    }

    if requeued > 0 || terminal > 0 {
        info!("Recovered stale jobs: {requeued} requeued, {terminal} terminal");
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
///
/// # Errors
///
/// Returns an error if the collection scan, upload cleanup, or hard-delete fails.
/// Do the transactional DB half of the retention purge (ref-count check +
/// decrement + hard delete). Returns the number of docs deleted and the field
/// maps of any deleted upload documents, whose files the CALLER must delete
/// **after committing** — so a crash/rollback leaves orphaned files (safe)
/// rather than DB rows pointing at deleted files (unsafe).
///
/// `conn` MUST be an IMMEDIATE transaction: the per-doc `get_ref_count_locked`
/// → `before_hard_delete` → `delete` sequence relies on it being atomic
/// (`SQLite` serializes writers; Postgres holds the `FOR UPDATE` lock until
/// commit) so a concurrent create can't increment a to-be-purged doc's
/// ref count between the read and the delete and leave a dangling reference.
pub fn purge_soft_deleted(
    conn: &dyn DbConnection,
    registry: &Registry,
    locale_config: &LocaleConfig,
) -> Result<(u64, Vec<DocumentFields>)> {
    let mut total = 0u64;
    let mut files_to_clean = Vec::new();

    for (slug, def) in &registry.collections {
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

        let (purged, mut files) = purge_collection(&PurgeCollectionInput {
            conn,
            slug,
            def,
            retention_seconds: seconds,
            locale_config,
        })?;
        total += purged;
        files_to_clean.append(&mut files);
    }

    Ok((total, files_to_clean))
}

/// Purge expired soft-deleted documents from a single collection.
///
/// Collects upload file data before deleting from DB, then removes files
/// from disk after the DB deletes succeed. A crash between DB delete and
/// file delete leaves orphaned files (safe), rather than orphaned DB records
/// pointing to deleted files (unsafe).
struct PurgeCollectionInput<'a> {
    conn: &'a dyn DbConnection,
    slug: &'a str,
    def: &'a CollectionDefinition,
    retention_seconds: i64,
    locale_config: &'a LocaleConfig,
}

fn purge_collection(p: &PurgeCollectionInput<'_>) -> Result<(u64, Vec<DocumentFields>)> {
    // Find docs past the retention threshold
    let (offset_sql, offset_param) = p.conn.date_offset_expr(p.retention_seconds, 1);
    let threshold_sql = format!(
        "SELECT id FROM \"{}\" WHERE _deleted_at IS NOT NULL \
         AND _deleted_at < {}",
        p.slug, offset_sql
    );
    let rows = p.conn.query_all(&threshold_sql, &[offset_param])?;

    let mut purged = 0u64;
    let mut upload_docs = Vec::new();

    for row in &rows {
        let id = match row.get_value(0) {
            Some(DbValue::Text(s)) => s.clone(),
            _ => continue,
        };

        // Skip documents that are still referenced -- protect referential integrity.
        // Uses locked variant to prevent concurrent creates from incrementing ref count
        // between this check and the DELETE (Postgres only; SQLite serializes via IMMEDIATE).
        let ref_count = query::ref_count::get_ref_count_locked(p.conn, p.slug, &id)?.unwrap_or(0);
        if ref_count > 0 {
            debug!(
                "Skipping purge of {}/{}: referenced by {} document(s)",
                p.slug, id, ref_count
            );
            continue;
        }

        // Decrement ref counts on targets before hard delete (CASCADE removes junction rows)
        query::ref_count::before_hard_delete(p.conn, p.slug, &id, &p.def.fields, p.locale_config)?;

        // Collect upload file field-maps BEFORE deleting from DB; the caller
        // deletes the actual files after committing the transaction.
        if p.def.is_upload_collection()
            && let Ok(Some(doc)) = query::find_by_id_unfiltered(p.conn, p.slug, p.def, &id, None)
        {
            upload_docs.push(doc.fields);
        }

        // Cancel pending image conversions — see
        // `core/upload/queue.rs::delete_image_jobs_for_document`. A failure
        // here only orphans a pending conversion job (it will no-op on a
        // missing doc), so log and continue rather than abort the purge.
        if p.def.is_upload_collection() {
            let _ = upload::delete_image_jobs_for_document(p.conn, p.slug, &id)
                .inspect_err(|e| warn!("Failed to cancel image jobs for {}/{}: {e}", p.slug, id));
        }

        // Clean up FTS index before hard delete
        if p.conn.supports_fts() {
            query::fts::fts_delete(p.conn, p.slug, &id)?;
        }

        // Hard delete the document from DB
        query::delete(p.conn, p.slug, &id)?;
        purged += 1;
    }

    if purged > 0 {
        info!(
            "Purged {} expired soft-deleted doc(s) from '{}'",
            purged, p.slug
        );
    }

    Ok((purged, upload_docs))
}

/// Dedup slug used to claim the retention-purge cron tick via
/// `_crap_cron_fired`. Retention purge is a "pseudo cron" job — it runs on a
/// fixed interval from the scheduler loop rather than a user-defined cron
/// expression, but must still be deduped across instances in multi-node
/// deployments.
pub(super) const RETENTION_PURGE_SLUG: &str = "__retention_purge";

/// Attempt to claim the retention-purge tick for this instance/window.
///
/// Returns `true` iff this caller won the tick and should run the purge.
/// Uses the same `_crap_cron_fired` dedup table as user cron jobs.
/// `window_seconds` must match the scheduler's purge cadence so two instances
/// firing inside the same window still end up with exactly one winner.
pub(super) fn claim_retention_purge_tick(
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
    use std::sync::Arc;

    use chrono::Timelike;
    use r2d2::Pool;
    use r2d2_sqlite::SqliteConnectionManager;

    use super::*;
    use crate::core::{Registry, job::JobStatus};

    // ── normalize_cron ────────────────────────────────────────────────────

    #[test]
    fn normalize_cron_prepends_seconds_to_5_field_expr() {
        // A 5-field (minute-granularity) cron gets a leading "0 " seconds field.
        assert_eq!(normalize_cron("*/5 * * * *"), "0 */5 * * * *");
    }

    #[test]
    fn normalize_cron_passes_through_6_field_expr() {
        // Already 6 fields (seconds present) → unchanged (whitespace collapsed).
        assert_eq!(normalize_cron("30 */5 * * * *"), "30 */5 * * * *");
    }

    #[test]
    fn normalize_cron_collapses_whitespace() {
        assert_eq!(normalize_cron("  */5   *  * * *  "), "0 */5 * * * *");
    }

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

    fn make_registry_with_jobs(jobs: Vec<JobDefinition>) -> Arc<Registry> {
        let shared = Registry::shared();
        {
            let mut reg = shared.write().unwrap();
            for job in jobs {
                reg.register_job(job);
            }
        }
        Registry::snapshot(&shared)
    }

    const TEST_STALE_THRESHOLD: u64 = 30;

    /// At-least-once: a dead (stale-heartbeat) job that still has retries left
    /// is REQUEUED (→ pending), so a surviving peer re-runs it.
    #[test]
    fn recover_requeues_retryable_stale_job() {
        let pool = make_test_pool();
        let conn = pool.get().unwrap();
        let registry = make_registry_with_jobs(vec![
            JobDefinition::builder("my_job", "some.handler").build(),
        ]);

        // Running at attempt 1 of 3, heartbeat 600s stale (worker died).
        job_query::insert_job(&conn, "my_job", "{}", "manual", 3, "default", 0).unwrap();
        conn.execute_batch(
            "UPDATE _crap_jobs SET status = 'running', attempt = 1, \
             heartbeat_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-600 seconds')",
        )
        .unwrap();

        recover_stale_jobs(&conn, &registry, TEST_STALE_THRESHOLD).unwrap();

        let pending =
            job_query::list_job_runs(&conn, None, Some(JobStatus::Pending), 100, 0).unwrap();
        assert_eq!(pending.len(), 1, "retryable stale job must be requeued");
        assert_eq!(
            job_query::list_job_runs(&conn, None, Some(JobStatus::Stale), 100, 0)
                .unwrap()
                .len(),
            0
        );
    }

    /// A dead job that has exhausted its retries goes terminal `stale`.
    #[test]
    fn recover_marks_exhausted_stale_job_terminal() {
        let pool = make_test_pool();
        let conn = pool.get().unwrap();
        let registry = make_registry_with_jobs(vec![
            JobDefinition::builder("my_job", "some.handler").build(),
        ]);

        // Running at attempt 1 of 1 (no retries left).
        job_query::insert_job(&conn, "my_job", "{}", "manual", 1, "default", 0).unwrap();
        conn.execute_batch(
            "UPDATE _crap_jobs SET status = 'running', attempt = 1, \
             heartbeat_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-600 seconds')",
        )
        .unwrap();

        recover_stale_jobs(&conn, &registry, TEST_STALE_THRESHOLD).unwrap();

        let stale = job_query::list_job_runs(&conn, None, Some(JobStatus::Stale), 100, 0).unwrap();
        assert_eq!(stale.len(), 1);
        assert!(
            stale[0]
                .error
                .as_ref()
                .unwrap()
                .contains("heartbeat expired")
        );
    }

    /// The multi-node fix: a running job with a FRESH heartbeat belongs to a
    /// live peer and must NOT be recovered.
    #[test]
    fn recover_ignores_fresh_heartbeat_job() {
        let pool = make_test_pool();
        let conn = pool.get().unwrap();
        let registry = make_registry_with_jobs(vec![
            JobDefinition::builder("my_job", "some.handler").build(),
        ]);

        job_query::insert_job(&conn, "my_job", "{}", "manual", 3, "default", 0).unwrap();
        conn.execute_batch(
            "UPDATE _crap_jobs SET status = 'running', attempt = 1, heartbeat_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
        )
        .unwrap();

        recover_stale_jobs(&conn, &registry, TEST_STALE_THRESHOLD).unwrap();

        // Untouched: still running, not requeued, not stale.
        assert_eq!(
            job_query::list_job_runs(&conn, None, Some(JobStatus::Running), 100, 0)
                .unwrap()
                .len(),
            1,
            "a live peer's fresh-heartbeat job must not be reclaimed"
        );
    }

    #[test]
    fn recover_ignores_pending_job() {
        let pool = make_test_pool();
        let conn = pool.get().unwrap();
        let registry = make_registry_with_jobs(vec![]);

        job_query::insert_job(&conn, "my_job", "{}", "manual", 1, "default", 0).unwrap();

        recover_stale_jobs(&conn, &registry, TEST_STALE_THRESHOLD).unwrap();

        assert_eq!(
            job_query::list_job_runs(&conn, None, Some(JobStatus::Stale), 100, 0)
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            job_query::list_job_runs(&conn, None, Some(JobStatus::Pending), 100, 0)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn recover_multiple_running() {
        let pool = make_test_pool();
        let conn = pool.get().unwrap();
        let registry = make_registry_with_jobs(vec![
            JobDefinition::builder("job_a", "handler_a").build(),
            JobDefinition::builder("job_b", "handler_b").build(),
        ]);

        // Both retryable + stale (null heartbeat) → both requeued.
        job_query::insert_job(&conn, "job_a", "{}", "manual", 3, "default", 0).unwrap();
        job_query::insert_job(&conn, "job_b", "{}", "manual", 3, "default", 0).unwrap();
        conn.execute_batch("UPDATE _crap_jobs SET status = 'running', attempt = 1")
            .unwrap();

        recover_stale_jobs(&conn, &registry, TEST_STALE_THRESHOLD).unwrap();

        assert_eq!(
            job_query::list_job_runs(&conn, None, Some(JobStatus::Pending), 100, 0)
                .unwrap()
                .len(),
            2
        );
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

        // Build the standard jobs schema via the canonical migration
        // path so we can't drift from production. `_crap_cron_fired`
        // is colocated here since these tests exercise the cron loop.
        let conn = pool.get().unwrap();
        crate::db::migrate::create_jobs_table(
            &conn,
            "TEXT DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
            "TEXT",
        )
        .expect("create_jobs_table");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS _crap_cron_fired (
                slug TEXT PRIMARY KEY,
                fired_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
            );",
        )
        .unwrap();
        drop(conn);

        pool
    }

    /// Regression: a failed job records the FULL anyhow cause chain (`{:#}`),
    /// not just the top-level message. The user-job path used to `to_string()`
    /// and silently drop the causes that the system-job paths kept; all job
    /// kinds now share `record_job_failure`, so the stored error is uniform.
    #[test]
    fn record_job_failure_stores_full_cause_chain() {
        let pool = make_test_pool();
        let conn = pool.get().unwrap();
        job_query::insert_job(&conn, "my_job", "{}", "manual", 3, "default", 0).unwrap();
        conn.execute_batch("UPDATE _crap_jobs SET status = 'running', attempt = 1")
            .unwrap();

        let job_run = job_query::list_job_runs(&conn, Some("my_job"), None, 1, 0)
            .unwrap()
            .pop()
            .expect("inserted job");
        drop(conn);

        let err = anyhow!("disk write failed").context("could not persist result");
        record_job_failure(&pool, &job_run, "Job (my_job)", &err).unwrap();

        let conn = pool.get().unwrap();
        let stored = job_query::list_job_runs(&conn, Some("my_job"), None, 1, 0)
            .unwrap()
            .pop()
            .and_then(|r| r.error)
            .expect("failure recorded an error");

        assert!(
            stored.contains("could not persist result"),
            "outer message missing: {stored}"
        );
        assert!(
            stored.contains("disk write failed"),
            "root cause dropped (not rendered with {{:#}}): {stored}"
        );
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

        check_cron_schedules(&pool, &registry, last_check, now, &HashMap::new()).unwrap();

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

        check_cron_schedules(&pool, &registry, last_check, now, &HashMap::new()).unwrap();

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

        check_cron_schedules(&pool, &registry, last_check, now, &HashMap::new()).unwrap();

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
            job_query::insert_job(&conn, "skip_job", "{}", "manual", 1, "default", 0).unwrap();
            conn.execute_batch("UPDATE _crap_jobs SET status = 'running'")
                .unwrap();
        }

        let now = chrono::Utc::now();
        let last_check = now - chrono::Duration::minutes(2);

        check_cron_schedules(&pool, &registry, last_check, now, &HashMap::new()).unwrap();

        // Should NOT insert a new pending job because skip_if_running=true and one is running
        let conn = pool.get().unwrap();
        let pending =
            job_query::list_job_runs(&conn, Some("skip_job"), Some(JobStatus::Pending), 100, 0)
                .unwrap();
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
            job_query::insert_job(&conn, "noskip_job", "{}", "manual", 1, "default", 0).unwrap();
            conn.execute_batch("UPDATE _crap_jobs SET status = 'running'")
                .unwrap();
        }

        let now = chrono::Utc::now();
        let last_check = now - chrono::Duration::minutes(2);

        check_cron_schedules(&pool, &registry, last_check, now, &HashMap::new()).unwrap();

        // Should insert a new pending job even though one is running
        let conn = pool.get().unwrap();
        let pending =
            job_query::list_job_runs(&conn, Some("noskip_job"), Some(JobStatus::Pending), 100, 0)
                .unwrap();
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
        check_cron_schedules(&pool, &registry, last_check, now, &HashMap::new()).unwrap();

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

    /// Regression: a job defined without `retries` inherits
    /// `[jobs.queues.<queue>] retries` at cron-fire time. Pre-Option<u32>
    /// migration this silently collapsed to `0` (one attempt); the
    /// queue config wins now.
    #[test]
    fn check_cron_schedules_inherits_queue_retries() {
        let pool = make_test_pool();
        let registry = make_registry_with_jobs(vec![
            JobDefinition::builder("inherits_cron", "some.handler")
                .schedule("* * * * *")
                // NO .retries() call — JobDefinition.retries = None,
                // so the queue's `retries = 5` should apply.
                .queue("reports")
                .skip_if_running(false)
                .build(),
        ]);

        let mut queue_retries = HashMap::new();
        queue_retries.insert("reports".to_string(), 5);

        let now = chrono::Utc::now();
        let last_check = now - chrono::Duration::minutes(2);

        check_cron_schedules(&pool, &registry, last_check, now, &queue_retries).unwrap();

        let conn = pool.get().unwrap();
        let jobs = job_query::list_job_runs(&conn, Some("inherits_cron"), None, 100, 0).unwrap();
        assert_eq!(jobs.len(), 1);
        // queue retries=5 → max_attempts = 5 + 1 = 6 (inherited)
        assert_eq!(
            jobs[0].max_attempts, 6,
            "JobDefinition without retries should inherit [jobs.queues.reports] retries = 5"
        );
    }

    /// Companion to `check_cron_schedules_inherits_queue_retries`:
    /// explicit `.retries(0)` BEATS the queue default (operator chose
    /// no retries even though the queue says 5).
    #[test]
    fn check_cron_schedules_explicit_zero_retries_overrides_queue() {
        let pool = make_test_pool();
        let registry = make_registry_with_jobs(vec![
            JobDefinition::builder("explicit_zero_cron", "some.handler")
                .schedule("* * * * *")
                .retries(0) // explicit "no retries"
                .queue("reports")
                .skip_if_running(false)
                .build(),
        ]);

        let mut queue_retries = HashMap::new();
        queue_retries.insert("reports".to_string(), 5);

        let now = chrono::Utc::now();
        let last_check = now - chrono::Duration::minutes(2);

        check_cron_schedules(&pool, &registry, last_check, now, &queue_retries).unwrap();

        let conn = pool.get().unwrap();
        let jobs =
            job_query::list_job_runs(&conn, Some("explicit_zero_cron"), None, 100, 0).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(
            jobs[0].max_attempts, 1,
            "explicit retries(0) must override the queue default of 5"
        );
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

        check_cron_schedules(&pool, &registry, last_check, now, &HashMap::new()).unwrap();

        let conn = pool.get().unwrap();
        let jobs = job_query::list_job_runs(&conn, Some("retried_cron"), None, 100, 0).unwrap();
        assert_eq!(jobs.len(), 1);
        // retries=3 => max_attempts = retries + 1 = 4
        assert_eq!(jobs[0].max_attempts, 4);
        assert_eq!(jobs[0].queue, "special");
    }
}
