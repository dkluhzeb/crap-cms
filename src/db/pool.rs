//! Database connection pool with backend-specific configuration.

#[cfg(feature = "sqlite")]
use anyhow::Context as _;
use anyhow::Result;
#[cfg(feature = "sqlite")]
use r2d2::Pool;
#[cfg(feature = "sqlite")]
use r2d2_sqlite::SqliteConnectionManager;
#[cfg(feature = "sqlite")]
use std::time::Duration;
use std::{path::Path, sync::Arc};

use crate::config::{CrapConfig, DatabaseBackend};

use super::connection::BoxedConnection;
#[cfg(feature = "sqlite")]
use crate::db::backend::sqlite::SqliteConnection;

/// Trait for pool backends.
///
/// Each backend (`SQLite`, `PostgreSQL`, ...) implements this once.
/// `DbPool` holds an `Arc<dyn PoolBackend>` and delegates `get()` to it.
pub(crate) trait PoolBackend: Send + Sync {
    fn get(&self) -> Result<BoxedConnection>;
    fn kind(&self) -> &'static str;
}

/// `SQLite` pool backend.
#[cfg(feature = "sqlite")]
struct SqlitePoolBackend {
    pool: Pool<SqliteConnectionManager>,
}

#[cfg(feature = "sqlite")]
impl PoolBackend for SqlitePoolBackend {
    fn get(&self) -> Result<BoxedConnection> {
        let conn = self.pool.get().context("Failed to get DB connection")?;
        Ok(BoxedConnection::new(Box::new(SqliteConnection::new(conn))))
    }

    fn kind(&self) -> &'static str {
        "sqlite"
    }
}

/// Connection pool — backend-agnostic wrapper.
///
/// Callers get a `BoxedConnection` from `pool.get()` and never see the
/// underlying backend. The pool type is chosen at startup via `create_pool`.
#[derive(Clone)]
pub struct DbPool {
    inner: Arc<dyn PoolBackend>,
}

impl DbPool {
    /// Get a connection from the pool.
    ///
    /// # Errors
    ///
    /// Returns an error if the pool is exhausted or the backend fails to
    /// hand out a connection.
    pub fn get(&self) -> Result<BoxedConnection> {
        self.inner.get()
    }

    /// Return the backend identifier (e.g. `"sqlite"`, `"postgres"`).
    #[must_use]
    pub fn kind(&self) -> &str {
        self.inner.kind()
    }

    /// Wrap an existing r2d2 `SQLite` pool. Used in tests.
    #[cfg(feature = "sqlite")]
    #[must_use]
    pub fn from_pool(pool: Pool<SqliteConnectionManager>) -> Self {
        Self {
            inner: Arc::new(SqlitePoolBackend { pool }),
        }
    }

    /// Create from an `Arc<dyn PoolBackend>` (used by backend-specific pool constructors).
    #[cfg(feature = "postgres")]
    pub(crate) fn from_backend(backend: Arc<dyn PoolBackend>) -> Self {
        Self { inner: backend }
    }
}

/// Create a connection pool based on the configured backend.
///
/// `config_dir` is used by the `SQLite` backend to resolve relative DB paths;
/// the Postgres backend ignores it (connection is fully URL-driven).
///
/// # Errors
///
/// Returns an error if the backend pool cannot be initialized (bad
/// configuration, unreachable database, missing feature flag, …).
pub fn create_pool(config_dir: &Path, config: &CrapConfig) -> Result<DbPool> {
    // Silence unused-param warning when built without the sqlite feature.
    let _ = config_dir;

    match config.database.backend {
        #[cfg(feature = "sqlite")]
        DatabaseBackend::Sqlite => create_sqlite_pool(config_dir, config),
        #[cfg(not(feature = "sqlite"))]
        DatabaseBackend::Sqlite => anyhow::bail!(
            "Database backend 'sqlite' requires the `sqlite` feature. Supported in this build: {}",
            supported_backends()
        ),
        #[cfg(feature = "postgres")]
        DatabaseBackend::Postgres => crate::db::backend::postgres::create_pool(config),
        #[cfg(not(feature = "postgres"))]
        DatabaseBackend::Postgres => anyhow::bail!(
            "Database backend 'postgres' requires the `postgres` feature. Supported in this build: {}",
            supported_backends()
        ),
    }
}

// Only referenced by the feature-disabled bail arms above, which don't exist
// when both backends are compiled in.
#[cfg(not(all(feature = "sqlite", feature = "postgres")))]
fn supported_backends() -> &'static str {
    #[cfg(all(feature = "sqlite", feature = "postgres"))]
    return "sqlite, postgres";
    #[cfg(all(feature = "sqlite", not(feature = "postgres")))]
    return "sqlite";
    #[cfg(all(not(feature = "sqlite"), feature = "postgres"))]
    return "postgres";
    #[cfg(not(any(feature = "sqlite", feature = "postgres")))]
    return "(none — enable the 'sqlite' or 'postgres' feature)";
}

/// Create a `SQLite` connection pool.
#[cfg(feature = "sqlite")]
fn create_sqlite_pool(config_dir: &Path, config: &CrapConfig) -> Result<DbPool> {
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
            cache_size: config.database.cache_size,
            mmap_size: config.database.mmap_size,
            wal_autocheckpoint: config.database.wal_autocheckpoint,
            stmt_cache_capacity: config.database.stmt_cache_capacity,
        }))
        .test_on_check_out(false)
        .build(manager)
        .context("Failed to create connection pool")?;

    Ok(DbPool::from_pool(pool))
}

#[cfg(feature = "sqlite")]
#[derive(Debug)]
struct SqlitePragmas {
    busy_timeout: u64,
    cache_size: i64,
    mmap_size: u64,
    wal_autocheckpoint: u32,
    stmt_cache_capacity: usize,
}

#[cfg(feature = "sqlite")]
impl r2d2::CustomizeConnection<rusqlite::Connection, rusqlite::Error> for SqlitePragmas {
    fn on_acquire(&self, conn: &mut rusqlite::Connection) -> Result<(), rusqlite::Error> {
        conn.execute_batch(&format!(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = {};
             PRAGMA wal_autocheckpoint = {};
             PRAGMA cache_size = {};
             PRAGMA mmap_size = {};
             PRAGMA temp_store = MEMORY;",
            self.busy_timeout, self.wal_autocheckpoint, self.cache_size, self.mmap_size
        ))?;
        // rusqlite's default prepared-statement cache holds 16 entries
        // per connection. With more than ~16 distinct SQL strings on
        // the hot path (find + per-doc hydrate joins + auth resolve),
        // the LRU evicts and the next call re-runs `sqlite3_prepare_v2`
        // → the query planner → SQLite's internal allocator (globally
        // locked). Profiling at concurrency 50 attributed ~53% of CPU
        // to `native_queued_spin_lock_slowpath` inside
        // `sqlite3LockAndPrepare` before this knob was wired up.
        // Configurable via `[database] stmt_cache_capacity`.
        conn.set_prepared_statement_cache_capacity(self.stmt_cache_capacity);

        // Disable SQLite's legacy "double-quoted string literal" misfeature so a
        // double-quoted token is *always* an identifier, matching Postgres. This
        // keeps quoted identifiers (needed for reserved-word column names) safe
        // AND preserves "no such column" errors: without it, `SELECT "missing"`
        // silently returns the string `missing` instead of erroring, masking
        // typos and the localized-read footgun.
        conn.set_db_config(rusqlite::config::DbConfig::SQLITE_DBCONFIG_DQS_DDL, false)?;
        conn.set_db_config(rusqlite::config::DbConfig::SQLITE_DBCONFIG_DQS_DML, false)?;

        Ok(())
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::config::CrapConfig;
    use crate::db::DbConnection;
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
        let row = conn
            .query_one("PRAGMA journal_mode", &[])
            .expect("PRAGMA journal_mode failed");
        let mode = row.unwrap().get_string("journal_mode").unwrap();
        assert_eq!(mode, "wal", "journal_mode should be WAL");
    }

    #[test]
    fn foreign_keys_are_enabled() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().expect("failed to get connection");
        let row = conn
            .query_one("PRAGMA foreign_keys", &[])
            .expect("PRAGMA foreign_keys failed");
        let fk = row.unwrap().get_i64("foreign_keys").unwrap();
        assert_eq!(fk, 1, "foreign_keys should be ON (1)");
    }

    #[test]
    fn synchronous_is_normal() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().expect("failed to get connection");
        let row = conn
            .query_one("PRAGMA synchronous", &[])
            .expect("PRAGMA synchronous failed");
        let sync = row.unwrap().get_i64("synchronous").unwrap();
        assert_eq!(sync, 1, "synchronous should be NORMAL (1)");
    }

    #[test]
    fn busy_timeout_is_applied() {
        let dir = TempDir::new().expect("failed to create temp dir");
        let mut config = CrapConfig::default();
        config.database.busy_timeout = 12345;
        let pool = create_pool(dir.path(), &config).expect("create_pool failed");
        let conn = pool.get().expect("failed to get connection");
        let row = conn
            .query_one("PRAGMA busy_timeout", &[])
            .expect("PRAGMA busy_timeout failed");
        let timeout = row.unwrap().get_i64("timeout").unwrap();
        assert_eq!(timeout, 12345, "busy_timeout should match configured value");
    }

    #[test]
    fn pool_kind_returns_sqlite() {
        let (_dir, pool) = temp_pool();
        assert_eq!(pool.kind(), "sqlite");
    }

    #[test]
    fn wal_autocheckpoint_is_set() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().expect("failed to get connection");
        let row = conn
            .query_one("PRAGMA wal_autocheckpoint", &[])
            .expect("PRAGMA wal_autocheckpoint failed");
        let checkpoint = row.unwrap().get_i64("wal_autocheckpoint").unwrap();
        assert_eq!(checkpoint, 1000, "wal_autocheckpoint should be 1000");
    }

    /// Production-critical: foreign-key cascade must fire when a parent
    /// row is deleted through a pooled connection. If this regresses,
    /// every hard-delete of a versioned document leaves orphan rows in
    /// `_versions_<collection>` that grow without bound. Mirrors the
    /// real `_versions_posts → posts(id) ON DELETE CASCADE` schema.
    #[test]
    fn fk_cascade_fires_on_pooled_connection_delete() {
        let (_dir, pool) = temp_pool();
        let conn = pool.get().expect("failed to get connection");

        conn.execute_batch(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE _versions_posts (
                id TEXT PRIMARY KEY,
                _parent TEXT NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
                snapshot TEXT
             );
             INSERT INTO posts (id) VALUES ('p1'), ('p2');
             INSERT INTO _versions_posts VALUES \
                ('v1', 'p1', '{}'), \
                ('v2', 'p1', '{}'), \
                ('v3', 'p2', '{}');",
        )
        .unwrap();

        let before = conn
            .query_one("SELECT COUNT(*) AS c FROM _versions_posts", &[])
            .unwrap()
            .unwrap()
            .get_i64("c")
            .unwrap();
        assert_eq!(before, 3, "fixture: 3 versions before delete");

        conn.execute("DELETE FROM posts WHERE id = 'p1'", &[])
            .unwrap();

        let after = conn
            .query_one("SELECT COUNT(*) AS c FROM _versions_posts", &[])
            .unwrap()
            .unwrap()
            .get_i64("c")
            .unwrap();

        assert_eq!(
            after, 1,
            "FK cascade must remove v1 + v2 when p1 is deleted; only v3 (p2's version) should remain"
        );
    }

    /// Sibling check: cascade fires *across* connection boundaries when
    /// a different pooled connection observes the parent table after
    /// the delete. Catches a class of "FK ON for the writer but OFF for
    /// the reader" misconfigurations that would let orphans accumulate
    /// in production where many connections share the pool.
    #[test]
    fn fk_cascade_visible_across_pool_connections() {
        let (_dir, pool) = temp_pool();
        let writer = pool.get().expect("writer connection");

        writer
            .execute_batch(
                "CREATE TABLE posts (id TEXT PRIMARY KEY);
                 CREATE TABLE _versions_posts (
                    id TEXT PRIMARY KEY,
                    _parent TEXT NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
                    snapshot TEXT
                 );
                 INSERT INTO posts (id) VALUES ('p1');
                 INSERT INTO _versions_posts VALUES ('v1', 'p1', '{}');",
            )
            .unwrap();

        writer
            .execute("DELETE FROM posts WHERE id = 'p1'", &[])
            .unwrap();

        // Drop the writer so r2d2 returns it to the pool, then read on
        // a freshly-acquired connection.
        drop(writer);

        let reader = pool.get().expect("reader connection");
        let count = reader
            .query_one("SELECT COUNT(*) AS c FROM _versions_posts", &[])
            .unwrap()
            .unwrap()
            .get_i64("c")
            .unwrap();
        assert_eq!(count, 0, "orphaned version row must be gone after cascade");
    }
}
