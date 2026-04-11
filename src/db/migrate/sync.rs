//! Top-level schema sync: creates system tables and syncs all collections/globals.

use anyhow::{Context as _, Result, anyhow};

use crate::{
    config::LocaleConfig,
    core::SharedRegistry,
    db::{DbConnection, DbPool},
};

use super::{backfill_ref_counts, collection, global};

/// Sync all collection tables with their Lua definitions.
///
/// Concurrency safety: `transaction_immediate()` acquires SQLite's write lock at
/// transaction start (not first write), so concurrent `sync_all` calls are serialized
/// by the database engine. Combined with `busy_timeout` (default 30s), the second caller
/// waits rather than failing.
pub fn sync_all(
    pool: &DbPool,
    registry: &SharedRegistry,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let mut conn = pool.get().context("Failed to get DB connection")?;
    let tx = conn
        .transaction_immediate()
        .context("Failed to start migration transaction")?;

    create_system_tables(&tx)?;

    let reg = registry
        .read()
        .map_err(|e| anyhow!("Registry lock poisoned: {}", e))?;

    for (slug, def) in &reg.collections {
        collection::sync_collection_table(&tx, slug, def, locale_config)?;
    }

    for (slug, def) in &reg.globals {
        global::sync_global_table(&tx, slug, def, locale_config)?;
    }

    backfill_ref_counts::backfill_if_needed(&tx, &reg, locale_config)?;

    drop(reg);
    tx.commit()
        .context("Failed to commit migration transaction")?;

    Ok(())
}

/// Create all system tables (_crap_meta, _crap_migrations, _crap_jobs, etc.).
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
    create_image_queue_table(conn, td, tt)?;

    Ok(())
}

/// Create the jobs table and ensure schema migrations.
fn create_jobs_table(tx: &dyn DbConnection, ts_default: &str, ts_type: &str) -> Result<()> {
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
            scheduled_by TEXT,
            created_at {ts_default},
            started_at {ts_type},
            completed_at {ts_type},
            heartbeat_at {ts_type},
            retry_after {ts_type}
        );
        CREATE INDEX IF NOT EXISTS idx_crap_jobs_status ON _crap_jobs(status);
        CREATE INDEX IF NOT EXISTS idx_crap_jobs_queue ON _crap_jobs(queue, status);
        CREATE INDEX IF NOT EXISTS idx_crap_jobs_slug ON _crap_jobs(slug, status);"
    ))
    .context("Failed to create _crap_jobs table")?;

    // Ensure retry_after column exists (added in 0.1.0-alpha.3)
    let job_cols = tx.get_table_columns("_crap_jobs")?;

    if !job_cols.contains("retry_after") {
        tx.execute_batch_ddl("ALTER TABLE _crap_jobs ADD COLUMN retry_after TEXT")
            .context("Failed to add retry_after column to _crap_jobs")?;
    }

    Ok(())
}

/// Create the image processing queue table.
fn create_image_queue_table(tx: &dyn DbConnection, ts_default: &str, ts_type: &str) -> Result<()> {
    tx.execute_batch_ddl(&format!(
        "CREATE TABLE IF NOT EXISTS _crap_image_queue (
            id TEXT PRIMARY KEY,
            collection TEXT NOT NULL,
            document_id TEXT NOT NULL,
            source_path TEXT NOT NULL,
            target_path TEXT NOT NULL,
            format TEXT NOT NULL,
            quality INTEGER NOT NULL,
            url_column TEXT NOT NULL,
            url_value TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'pending',
            error TEXT,
            created_at {ts_default},
            completed_at {ts_type}
        );
        CREATE INDEX IF NOT EXISTS idx_crap_image_queue_status ON _crap_image_queue(status);"
    ))
    .context("Failed to create _crap_image_queue table")
}
