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

/// Connection pool — backend-agnostic wrapper over a **read** and a
/// **write** pool.
///
/// Callers get a `BoxedConnection` from [`DbPool::get`] (read) or
/// [`DbPool::write`] (write) and never see the underlying backend. The
/// backend is chosen at startup via [`create_pool`].
///
/// Under `SQLite` WAL, an unlimited number of readers run concurrently but
/// there is a single writer. Serving reads and writes from one shared pool
/// lets a burst of writers consume every connection and starve readers.
/// Splitting them — a large read pool and a small separate write pool —
/// keeps read concurrency independent of write load: excess writers queue
/// on write-pool checkout rather than on read connections. On Postgres the
/// two share one backend (MVCC already handles concurrent writers).
#[derive(Clone)]
pub struct DbPool {
    read: Arc<dyn PoolBackend>,
    write: Arc<dyn PoolBackend>,
}

impl DbPool {
    /// Get a connection from the **read** pool.
    ///
    /// This is the default acquisition method. Any path that opens a write
    /// transaction (`BEGIN IMMEDIATE` via
    /// [`transaction_immediate`](crate::db::DbConnection::transaction_immediate))
    /// must use [`DbPool::write`] instead, so it does not consume a read
    /// connection and starve concurrent readers.
    ///
    /// # Errors
    ///
    /// Returns an error if the pool is exhausted or the backend fails to
    /// hand out a connection.
    pub fn get(&self) -> Result<BoxedConnection> {
        self.read.get()
    }

    /// Get a connection from the **write** pool.
    ///
    /// Callers that open `BEGIN IMMEDIATE` acquire here. On `SQLite` the write
    /// pool is small and separate from the read pool; on Postgres it is the
    /// same backend as [`DbPool::get`].
    ///
    /// # Errors
    ///
    /// Returns an error if the pool is exhausted or the backend fails to
    /// hand out a connection.
    pub fn write(&self) -> Result<BoxedConnection> {
        self.write.get()
    }

    /// Return the backend identifier (e.g. `"sqlite"`, `"postgres"`).
    #[must_use]
    pub fn kind(&self) -> &str {
        self.read.kind()
    }

    /// Wrap an existing r2d2 `SQLite` pool as an unsplit pool (reads and
    /// writes share one backend). Used in tests.
    #[cfg(feature = "sqlite")]
    #[must_use]
    pub fn from_pool(pool: Pool<SqliteConnectionManager>) -> Self {
        let backend: Arc<dyn PoolBackend> = Arc::new(SqlitePoolBackend { pool });
        Self {
            read: Arc::clone(&backend),
            write: backend,
        }
    }

    /// Build a split pool from separate read and write backends.
    #[cfg(feature = "sqlite")]
    fn from_split(read: Arc<dyn PoolBackend>, write: Arc<dyn PoolBackend>) -> Self {
        Self { read, write }
    }

    /// Create from a single `Arc<dyn PoolBackend>` shared by reads and
    /// writes (used by the Postgres backend, which does not split).
    #[cfg(feature = "postgres")]
    pub(crate) fn from_backend(backend: Arc<dyn PoolBackend>) -> Self {
        Self {
            read: Arc::clone(&backend),
            write: backend,
        }
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

    // Reads and writes get separate pools over the same WAL database (see
    // `DbPool`). A large read pool keeps read concurrency independent of a
    // small write pool that serializes on SQLite's single writer.
    //
    // Neither pool keeps a minimum-idle connection (`min_idle = None`). A
    // freshly built r2d2 pool that eagerly opens a min-idle connection can
    // deadlock in `Drop` when the pool is short-lived: closing that idle
    // connection triggers a WAL checkpoint that blocks on a lock still held by
    // an outstanding connection from the sibling pool. Under the test suite —
    // where every test builds and drops its own split pool concurrently — this
    // manifests as an intermittent hang. `min_idle = None` (connections created
    // on demand) removes the idle connection and the checkpoint-on-drop, for
    // both pools symmetrically.
    let read = build_sqlite_pool(&db_path, config.database.pool_max_size, None, config)?;
    let write = build_sqlite_pool(&db_path, config.database.write_pool_max_size, None, config)?;

    tracing::info!(
        "SQLite pools created (read={}, write={})",
        config.database.pool_max_size,
        config.database.write_pool_max_size
    );

    Ok(DbPool::from_split(
        Arc::new(SqlitePoolBackend { pool: read }),
        Arc::new(SqlitePoolBackend { pool: write }),
    ))
}

/// Build one `SQLite` r2d2 pool of the given size, wired with the shared
/// pragmas. Reads and writes each get their own pool via this helper.
#[cfg(feature = "sqlite")]
fn build_sqlite_pool(
    db_path: &Path,
    max_size: u32,
    min_idle: Option<u32>,
    config: &CrapConfig,
) -> Result<Pool<SqliteConnectionManager>> {
    let manager = SqliteConnectionManager::file(db_path);

    Pool::builder()
        .max_size(max_size)
        .min_idle(min_idle)
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
        .context("Failed to create connection pool")
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
    fn write_pool_hands_out_usable_connections() {
        let (_dir, pool) = temp_pool();
        let conn = pool.write().expect("failed to get write connection");
        drop(conn);
    }

    #[test]
    fn read_and_write_pools_share_the_same_database() {
        let (_dir, pool) = temp_pool();

        // Write through the write pool...
        let wconn = pool.write().expect("write connection");
        wconn
            .execute_batch(
                "CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT);
                 INSERT INTO t (v) VALUES ('hello');",
            )
            .expect("write via write pool");
        drop(wconn);

        // ...and read it back through the read pool.
        let rconn = pool.get().expect("read connection");
        let v = rconn
            .query_one("SELECT v FROM t WHERE id = 1", &[])
            .expect("select")
            .unwrap()
            .get_string("v")
            .unwrap();
        assert_eq!(v, "hello", "read pool must see what the write pool wrote");
    }

    /// The read and write pools must be *independent*: exhausting the write
    /// pool must not block reads. With a write pool of size 1, holding its
    /// one connection makes a second `write()` time out, while `get()` (the
    /// large read pool) still succeeds immediately. This is the property the
    /// whole split exists to guarantee — writers cannot starve readers.
    #[test]
    fn exhausting_the_write_pool_does_not_block_reads() {
        let dir = TempDir::new().expect("failed to create temp dir");
        let mut config = CrapConfig::default();
        config.database.pool_max_size = 8;
        config.database.write_pool_max_size = 1;
        config.database.connection_timeout = 1; // keep the exhaustion wait short
        let pool = create_pool(dir.path(), &config).expect("create_pool failed");

        // Hold the write pool's only connection.
        let _held = pool.write().expect("first write connection");

        // A second write connection cannot be obtained (write pool exhausted).
        assert!(
            pool.write().is_err(),
            "write pool of size 1 must be exhausted while its connection is held"
        );

        // But a read connection is still available — reads are not blocked
        // by a saturated write pool.
        let rconn = pool.get().expect("read connection must still be available");
        drop(rconn);
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
