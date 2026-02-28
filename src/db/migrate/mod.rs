//! Dynamic schema migration: syncs SQLite tables to match Lua collection definitions.

mod collection;
mod global;
mod helpers;
mod tracking;

pub use tracking::{
    list_migration_files, get_applied_migrations, get_applied_migrations_desc,
    get_pending_migrations, record_migration, remove_migration, drop_all_tables,
};

use anyhow::{Context, Result};

use crate::config::LocaleConfig;
use crate::core::SharedRegistry;
use super::DbPool;

/// Sync all collection tables with their Lua definitions.
pub fn sync_all(pool: &DbPool, registry: &SharedRegistry, locale_config: &LocaleConfig) -> Result<()> {
    let mut conn = pool.get().context("Failed to get DB connection")?;
    let tx = conn.transaction().context("Failed to start migration transaction")?;

    // Create metadata table
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS _crap_meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL,
            updated_at TEXT DEFAULT (datetime('now'))
        );"
    ).context("Failed to create _crap_meta table")?;

    // Create migrations tracking table
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS _crap_migrations (
            filename TEXT PRIMARY KEY,
            applied_at TEXT DEFAULT (datetime('now'))
        );"
    ).context("Failed to create _crap_migrations table")?;

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
        CREATE INDEX IF NOT EXISTS idx_crap_jobs_slug ON _crap_jobs(slug, status);"
    ).context("Failed to create _crap_jobs table")?;

    let reg = registry.read()
        .map_err(|e| anyhow::anyhow!("Registry lock poisoned: {}", e))?;

    for (slug, def) in &reg.collections {
        collection::sync_collection_table(&tx, slug, def, locale_config)?;
    }

    for (slug, def) in &reg.globals {
        global::sync_global_table(&tx, slug, def, locale_config)?;
    }

    drop(reg);
    tx.commit().context("Failed to commit migration transaction")?;

    Ok(())
}
