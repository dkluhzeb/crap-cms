//! Top-level schema sync: creates system tables and syncs all collections/globals.

use anyhow::{Context as _, Result};

use crate::{
    config::LocaleConfig,
    core::Registry,
    db::{DbConnection, DbPool},
};

use super::{backfill_ref_counts, collection, global};

/// Sync all collection tables with their Lua definitions.
///
/// Concurrency safety: `transaction_immediate()` acquires `SQLite`'s write lock at
/// transaction start (not first write), so concurrent `sync_all` calls are serialized
/// by the database engine. Combined with `busy_timeout` (default 30s), the second caller
/// waits rather than failing.
///
/// # Errors
///
/// Returns an error if the connection, transaction, or any of the
/// per-collection/global schema-sync steps fails.
pub fn sync_all(pool: &DbPool, registry: &Registry, locale_config: &LocaleConfig) -> Result<()> {
    let mut conn = pool.get().context("Failed to get DB connection")?;
    let tx = conn
        .transaction_immediate()
        .context("Failed to start migration transaction")?;

    create_system_tables(&tx)?;

    for (slug, def) in &registry.collections {
        collection::sync_collection_table(&tx, slug, def, locale_config)?;
    }

    for (slug, def) in &registry.globals {
        global::sync_global_table(&tx, slug, def, locale_config)?;
    }

    backfill_ref_counts::backfill_if_needed(&tx, registry, locale_config)?;

    tx.commit()
        .context("Failed to commit migration transaction")?;

    Ok(())
}

/// Create all system tables (_`crap_meta`, _`crap_migrations`, _`crap_jobs`, etc.).
fn create_system_tables(conn: &dyn DbConnection) -> Result<()> {
    let td = conn.timestamp_column_default();
    let tt = conn.timestamp_column_type();

    conn.execute_batch_ddl(&format!(
        "CREATE TABLE IF NOT EXISTS _crap_meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL,
            updated_at {td}
        );"
    ))
    .context("Failed to create _crap_meta table")?;

    conn.execute_batch_ddl(&format!(
        "CREATE TABLE IF NOT EXISTS _crap_migrations (
            filename TEXT PRIMARY KEY,
            applied_at {td}
        );"
    ))
    .context("Failed to create _crap_migrations table")?;

    conn.execute_batch_ddl(&format!(
        "CREATE TABLE IF NOT EXISTS _crap_cron_fired (
            slug TEXT PRIMARY KEY,
            fired_at {td}
        );"
    ))
    .context("Failed to create _crap_cron_fired table")?;

    conn.execute_batch_ddl(
        "CREATE TABLE IF NOT EXISTS _crap_user_settings (
            user_id TEXT PRIMARY KEY,
            settings TEXT NOT NULL DEFAULT '{}'
        );",
    )
    .context("Failed to create _crap_user_settings table")?;

    create_jobs_table(conn, td, tt)?;

    // alpha.9: image conversion moved into the unified job queue.
    // Drain any in-flight rows from the legacy `_crap_image_queue` into
    // `_crap_jobs` as `_system_image_convert` system jobs, then drop
    // the legacy table. Idempotent — skips if the table is already
    // gone.
    drain_legacy_image_queue(conn)?;

    Ok(())
}

/// Create the jobs table and ensure schema migrations.
///
/// Single source of truth for the `_crap_jobs` schema — production
/// migration AND test setups (`test_helpers::setup_db`,
/// `scheduler/runner.rs` tests) call this directly so the schema
/// can't drift between paths. The `CREATE TABLE IF NOT EXISTS` makes
/// it safe to call on already-migrated databases; subsequent
/// `ALTER ADD COLUMN` blocks are also idempotent.
pub(crate) fn create_jobs_table(
    tx: &dyn DbConnection,
    ts_default: &str,
    ts_type: &str,
) -> Result<()> {
    tx.execute_batch_ddl(&format!(
        "CREATE TABLE IF NOT EXISTS _crap_jobs (
            id TEXT PRIMARY KEY,
            slug TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'pending',
            queue TEXT NOT NULL DEFAULT 'default',
            data TEXT DEFAULT '{{}}',
            result TEXT,
            error TEXT,
            attempt INTEGER NOT NULL DEFAULT 0,
            max_attempts INTEGER NOT NULL DEFAULT 1,
            priority INTEGER NOT NULL DEFAULT 0,
            unique_key TEXT,
            scheduled_by TEXT,
            created_at {ts_default},
            started_at {ts_type},
            completed_at {ts_type},
            heartbeat_at {ts_type},
            retry_after {ts_type}
        );
        CREATE INDEX IF NOT EXISTS idx_crap_jobs_status ON _crap_jobs(status);
        CREATE INDEX IF NOT EXISTS idx_crap_jobs_queue ON _crap_jobs(queue, status);
        CREATE INDEX IF NOT EXISTS idx_crap_jobs_slug ON _crap_jobs(slug, status);
        CREATE INDEX IF NOT EXISTS idx_crap_jobs_priority
            ON _crap_jobs(status, queue, priority DESC, created_at);
        CREATE UNIQUE INDEX IF NOT EXISTS idx_crap_jobs_unique_active
            ON _crap_jobs(slug, unique_key)
            WHERE unique_key IS NOT NULL AND status IN ('pending', 'running');"
    ))
    .context("Failed to create _crap_jobs table")?;

    // Ensure retry_after column exists (added in 0.1.0-alpha.3)
    let job_cols = tx.get_table_columns("_crap_jobs")?;

    if !job_cols.contains("retry_after") {
        tx.execute_batch_ddl("ALTER TABLE _crap_jobs ADD COLUMN retry_after TEXT")
            .context("Failed to add retry_after column to _crap_jobs")?;
    }

    // Ensure priority column exists (added in 0.1.0-alpha.9)
    if !job_cols.contains("priority") {
        tx.execute_batch_ddl(
            "ALTER TABLE _crap_jobs ADD COLUMN priority INTEGER NOT NULL DEFAULT 0",
        )
        .context("Failed to add priority column to _crap_jobs")?;
        tx.execute_batch_ddl(
            "CREATE INDEX IF NOT EXISTS idx_crap_jobs_priority \
             ON _crap_jobs(status, queue, priority DESC, created_at)",
        )
        .context("Failed to create priority index on _crap_jobs")?;
    }

    // Ensure unique_key column + partial unique index exist (added in
    // 0.1.0-alpha.9). The partial index makes `(slug, unique_key)`
    // unique among pending+running rows only — completed/failed runs
    // don't block re-enqueue of the same logical task.
    if !job_cols.contains("unique_key") {
        tx.execute_batch_ddl("ALTER TABLE _crap_jobs ADD COLUMN unique_key TEXT")
            .context("Failed to add unique_key column to _crap_jobs")?;
        tx.execute_batch_ddl(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_crap_jobs_unique_active \
             ON _crap_jobs(slug, unique_key) \
             WHERE unique_key IS NOT NULL AND status IN ('pending', 'running')",
        )
        .context("Failed to create unique_key index on _crap_jobs")?;
    }

    Ok(())
}

/// One-time alpha.9 migration: move any non-completed
/// `_crap_image_queue` rows into the unified `_crap_jobs` table as
/// `_system_image_convert` system jobs, then drop the legacy table.
///
/// Idempotent: returns early if `_crap_image_queue` doesn't exist.
fn drain_legacy_image_queue(conn: &dyn DbConnection) -> Result<()> {
    use crate::core::upload::{IMAGE_CONVERT_QUEUE, ImageConvertJobData, SYSTEM_IMAGE_CONVERT_JOB};

    // SQLite-only check; on Postgres this code path never fires
    // because alpha.9 schemas there were never deployed.
    if conn.kind() != "sqlite" {
        return Ok(());
    }

    let Ok(rows) = conn.query_all(
        "SELECT name FROM sqlite_master WHERE type='table' AND name='_crap_image_queue'",
        &[],
    ) else {
        return Ok(());
    };
    if rows.is_empty() {
        return Ok(());
    }

    // Move every non-completed row into `_crap_jobs`. We deliberately
    // include status='processing' too — it gets a fresh pending job in
    // the new queue (the legacy worker isn't running anymore to
    // complete it).
    let pending = conn.query_all(
        "SELECT collection, document_id, source_path, target_path, format, \
                quality, url_column, url_value \
         FROM _crap_image_queue \
         WHERE status IN ('pending', 'processing', 'failed')",
        &[],
    )?;

    let mut drained = 0u64;
    for row in &pending {
        let data = ImageConvertJobData {
            collection: row.text_at(0).unwrap_or_default().to_string(),
            document_id: row.text_at(1).unwrap_or_default().to_string(),
            source_path: row.text_at(2).unwrap_or_default().to_string(),
            target_path: row.text_at(3).unwrap_or_default().to_string(),
            format: row.text_at(4).unwrap_or_default().to_string(),
            quality: u8::try_from(row.i64_at(5).unwrap_or(80)).unwrap_or(80),
            url_column: row.text_at(6).unwrap_or_default().to_string(),
            url_value: row.text_at(7).unwrap_or_default().to_string(),
        };
        let data_json = serde_json::to_string(&data)
            .context("Failed to serialize image-convert job during legacy drain")?;

        crate::db::query::jobs::insert_job(
            conn,
            SYSTEM_IMAGE_CONVERT_JOB,
            &data_json,
            "system",
            3, // default max_attempts for image conversion
            IMAGE_CONVERT_QUEUE,
            0,
        )
        .context("Failed to insert drained image-convert job")?;

        drained += 1;
    }

    conn.execute_batch_ddl("DROP TABLE IF EXISTS _crap_image_queue")
        .context("Failed to drop legacy _crap_image_queue table")?;

    if drained > 0 {
        tracing::info!(
            "Drained {} legacy image-queue entries into _crap_jobs (table dropped)",
            drained
        );
    }

    Ok(())
}
