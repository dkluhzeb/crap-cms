//! SQLite connection pool with WAL mode and tuned pragmas.

use anyhow::{Context as _, Result};
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use std::path::Path;
use std::time::Duration;

use crate::config::CrapConfig;

/// r2d2 connection pool for SQLite.
pub type DbPool = Pool<SqliteConnectionManager>;

/// Create a connection pool, ensuring the database directory exists.
pub fn create_pool(config_dir: &Path, config: &CrapConfig) -> Result<DbPool> {
    let db_path = config.db_path(config_dir);

    // Ensure parent directory exists
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!("Failed to create database directory: {}", parent.display())
        })?;
    }

    tracing::info!("Database path: {}", db_path.display());

    let manager = SqliteConnectionManager::file(&db_path);

    let pool = Pool::builder()
        .max_size(config.database.pool_max_size)
        .min_idle(Some(1))
        .connection_timeout(Duration::from_secs(config.database.connection_timeout))
        .connection_customizer(Box::new(SqlitePragmas {
            busy_timeout: config.database.busy_timeout,
        }))
        .test_on_check_out(false)
        .build(manager)
        .context("Failed to create connection pool")?;

    Ok(pool)
}

#[derive(Debug)]
struct SqlitePragmas {
    busy_timeout: u64,
}

impl r2d2::CustomizeConnection<rusqlite::Connection, rusqlite::Error> for SqlitePragmas {
    fn on_acquire(&self, conn: &mut rusqlite::Connection) -> Result<(), rusqlite::Error> {
        conn.execute_batch(&format!(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = {};
             PRAGMA wal_autocheckpoint = 1000;",
            self.busy_timeout
        ))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CrapConfig;
    use tempfile::TempDir;

    fn temp_pool() -> (TempDir, DbPool) {
        let dir = TempDir::new().expect("failed to create temp dir");
        let config = CrapConfig::default();
        let pool = create_pool(dir.path(), &config).expect("create_pool failed");
        (dir, pool)
    }

    #[test]
    fn create_pool_succeeds_with_temp_dir() {
        let (_dir, pool) = temp_pool();
        // A connection should be obtainable from the pool.
        let conn = pool.get().expect("failed to get connection from pool");
        drop(conn);
    }

    #[test]
    fn creates_database_directory_if_missing() {
        let dir = TempDir::new().expect("failed to create temp dir");
        // Point the db at a nested subdirectory that does not yet exist.
        let mut config = CrapConfig::default();
        config.database.path = "nested/sub/crap.db".to_string();
        let pool = create_pool(dir.path(), &config).expect("create_pool failed");
        let conn = pool.get().expect("failed to get connection");
        drop(conn);
        assert!(dir.path().join("nested/sub/crap.db").exists());
    }

    #[test]
    fn wal_mode_is_set() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().expect("failed to get connection");
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .expect("PRAGMA journal_mode failed");
        assert_eq!(mode, "wal", "journal_mode should be WAL");
    }

    #[test]
    fn foreign_keys_are_enabled() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().expect("failed to get connection");
        let fk: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .expect("PRAGMA foreign_keys failed");
        assert_eq!(fk, 1, "foreign_keys should be ON (1)");
    }

    #[test]
    fn synchronous_is_normal() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().expect("failed to get connection");
        // SQLite returns synchronous as an integer: 0=OFF, 1=NORMAL, 2=FULL, 3=EXTRA.
        let sync: i64 = conn
            .query_row("PRAGMA synchronous", [], |row| row.get(0))
            .expect("PRAGMA synchronous failed");
        assert_eq!(sync, 1, "synchronous should be NORMAL (1)");
    }

    #[test]
    fn busy_timeout_is_applied() {
        let dir = TempDir::new().expect("failed to create temp dir");
        let mut config = CrapConfig::default();
        config.database.busy_timeout = 12345;
        let pool = create_pool(dir.path(), &config).expect("create_pool failed");
        let conn = pool.get().expect("failed to get connection");
        let timeout: i64 = conn
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .expect("PRAGMA busy_timeout failed");
        assert_eq!(timeout, 12345, "busy_timeout should match configured value");
    }

    #[test]
    fn wal_autocheckpoint_is_set() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().expect("failed to get connection");
        let checkpoint: i64 = conn
            .query_row("PRAGMA wal_autocheckpoint", [], |row| row.get(0))
            .expect("PRAGMA wal_autocheckpoint failed");
        assert_eq!(checkpoint, 1000, "wal_autocheckpoint should be 1000");
    }
}
