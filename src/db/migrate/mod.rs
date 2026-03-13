//! Dynamic schema migration: syncs SQLite tables to match Lua collection definitions.

mod collection;
mod global;
pub mod helpers;
mod tracking;

pub use tracking::{
    drop_all_tables, get_applied_migrations, get_applied_migrations_desc, get_pending_migrations,
    list_migration_files, record_migration, remove_migration,
};

use anyhow::{Context as _, Result, anyhow};

use super::DbPool;
use crate::{config::LocaleConfig, core::SharedRegistry};

/// Sync all collection tables with their Lua definitions.
pub fn sync_all(
    pool: &DbPool,
    registry: &SharedRegistry,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let mut conn = pool.get().context("Failed to get DB connection")?;
    let tx = conn
        .transaction()
        .context("Failed to start migration transaction")?;

    // Create metadata table
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS _crap_meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL,
            updated_at TEXT DEFAULT (datetime('now'))
        );",
    )
    .context("Failed to create _crap_meta table")?;

    // Create migrations tracking table
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS _crap_migrations (
            filename TEXT PRIMARY KEY,
            applied_at TEXT DEFAULT (datetime('now'))
        );",
    )
    .context("Failed to create _crap_migrations table")?;

    // Create jobs table
    tx.execute_batch(
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
    .context("Failed to create _crap_jobs table")?;

    // Create user settings table (decoupled from auth collections)
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS _crap_user_settings (
            user_id TEXT PRIMARY KEY,
            settings TEXT NOT NULL DEFAULT '{}'
        );",
    )
    .context("Failed to create _crap_user_settings table")?;

    // Create image processing queue table
    tx.execute_batch(
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
            created_at TEXT DEFAULT (datetime('now')),
            completed_at TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_crap_image_queue_status ON _crap_image_queue(status);",
    )
    .context("Failed to create _crap_image_queue table")?;

    let reg = registry
        .read()
        .map_err(|e| anyhow!("Registry lock poisoned: {}", e))?;

    for (slug, def) in &reg.collections {
        collection::sync_collection_table(&tx, slug, def, locale_config)?;
    }

    for (slug, def) in &reg.globals {
        global::sync_global_table(&tx, slug, def, locale_config)?;
    }

    drop(reg);
    tx.commit()
        .context("Failed to commit migration transaction")?;

    Ok(())
}
