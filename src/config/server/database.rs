//! `SQLite` / `PostgreSQL` backend, pool, and per-engine tuning settings.

use serde::{Deserialize, Serialize};

use crate::config::parsing::{serde_duration, serde_duration_ms};

/// Database backend engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DatabaseBackend {
    /// `SQLite` (the default). Requires the `sqlite` feature.
    #[default]
    Sqlite,
    /// `PostgreSQL`. Requires the `postgres` feature.
    Postgres,
}

/// `SQLite` database path and pool configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct DatabaseConfig {
    /// Database backend: `sqlite` (default) or `postgres`.
    pub backend: DatabaseBackend,
    /// `PostgreSQL` connection URL (only used when `backend = "postgres"`).
    /// e.g., `"host=localhost user=crap dbname=crap_cms"`
    #[serde(default)]
    pub url: Option<String>,
    /// Path to the `SQLite` database file (only used when `backend = "sqlite"`).
    pub path: String,
    /// Maximum number of connections in the **read** pool. Default: 64.
    ///
    /// Reads and writes draw from separate pools (see `write_pool_max_size`).
    /// Under `SQLite` WAL an unlimited number of readers run concurrently, so
    /// this is the pool that governs read concurrency; size it to the peak
    /// number of simultaneous read requests you want to serve without
    /// queueing. (Historically this sized the single shared pool; it now
    /// sizes the read pool, which is where read concurrency is decided.)
    pub pool_max_size: u32,
    /// Maximum number of connections in the **write** pool. Default: 4.
    ///
    /// Writes take `BEGIN IMMEDIATE` (`SQLite` WAL serializes to a single
    /// writer), so a small pool is correct: excess concurrent writers queue
    /// on pool checkout rather than consuming read-pool connections and
    /// starving readers. Raising this does not increase `SQLite` write
    /// throughput (the engine still serializes writers); it only widens how
    /// many writers wait on a connection vs. on the write lock. On Postgres,
    /// where writers run concurrently, raise it to your write concurrency.
    #[serde(default = "default_write_pool_max_size")]
    pub write_pool_max_size: u32,
    /// `SQLite` busy timeout in milliseconds. Default: 30000 (30s).
    /// Accepts integer milliseconds or human-readable string ("30s", "1m").
    #[serde(with = "serde_duration_ms")]
    pub busy_timeout: u64,
    /// Pool connection timeout in seconds. Default: 30.
    /// Accepts integer seconds or human-readable string ("30s", "1m").
    ///
    /// Should be >= `busy_timeout` so `SQLite`'s own retry loop for write
    /// contention (WAL single-writer) gets a chance to resolve before the
    /// outer pool-level timeout fires. A shorter value will cause spurious
    /// `ServiceError::Transient` errors under load on `find_deep` or bulk
    /// write workloads where requests issue many back-to-back queries.
    #[serde(with = "serde_duration")]
    pub connection_timeout: u64,
    /// `SQLite` page cache size in KB. Negative = KB, positive = pages. Default: 16384 (16MB).
    /// Higher values improve read performance for large datasets.
    #[serde(default = "default_cache_size")]
    pub cache_size: i64,
    /// `SQLite` memory-mapped I/O size in bytes. Default: 268435456 (256MB).
    /// Set to 0 to disable. Improves read throughput for databases smaller than this value.
    #[serde(default = "default_mmap_size")]
    pub mmap_size: u64,
    /// `SQLite` WAL auto-checkpoint threshold in pages. Default: 1000.
    /// Lower values keep the WAL file smaller; higher values reduce checkpoint frequency.
    #[serde(default = "default_wal_autocheckpoint")]
    pub wal_autocheckpoint: u32,
    /// rusqlite per-connection prepared-statement cache capacity.
    /// Default: 128.
    ///
    /// rusqlite's built-in default is 16. With more than ~16 distinct
    /// SQL strings on the hot path (find + per-doc hydrate joins +
    /// auth resolve), the LRU evicts and the next call re-runs
    /// `sqlite3_prepare_v2` → the query planner → `SQLite`'s internal
    /// allocator (globally locked). Profiling at concurrency 50
    /// attributed ~53% of CPU to `native_queued_spin_lock_slowpath`
    /// inside `sqlite3LockAndPrepare` before this knob existed.
    /// 128 covers the current workload comfortably; bump it if a
    /// hot path adds enough new SQL variants to thrash again.
    #[serde(default = "default_stmt_cache_capacity")]
    pub stmt_cache_capacity: usize,
}

fn default_write_pool_max_size() -> u32 {
    4
}

fn default_cache_size() -> i64 {
    -16384
}

fn default_mmap_size() -> u64 {
    268_435_456
}

fn default_wal_autocheckpoint() -> u32 {
    1000
}

fn default_stmt_cache_capacity() -> usize {
    128
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            backend: DatabaseBackend::default(),
            url: None,
            path: "data/crap.db".to_string(),
            pool_max_size: 64,
            write_pool_max_size: default_write_pool_max_size(),
            busy_timeout: 30000,
            connection_timeout: 30,
            cache_size: default_cache_size(),
            mmap_size: default_mmap_size(),
            wal_autocheckpoint: default_wal_autocheckpoint(),
            stmt_cache_capacity: default_stmt_cache_capacity(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::config::CrapConfig;

    #[test]
    fn database_config_from_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(
            tmp.path().join("crap.toml"),
            "[database]\npool_max_size = 32\nbusy_timeout = 60000\n",
        )
        .unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.database.pool_max_size, 32);
        assert_eq!(config.database.busy_timeout, 60000);
    }

    #[test]
    fn stmt_cache_capacity_defaults_to_128() {
        let config = crate::config::CrapConfig::default();
        assert_eq!(config.database.stmt_cache_capacity, 128);
    }

    #[test]
    fn write_pool_max_size_defaults_to_4() {
        let config = crate::config::CrapConfig::default();
        assert_eq!(config.database.write_pool_max_size, 4);
    }

    #[test]
    fn write_pool_max_size_from_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(
            tmp.path().join("crap.toml"),
            "[database]\nwrite_pool_max_size = 8\n",
        )
        .unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.database.write_pool_max_size, 8);
    }

    #[test]
    fn stmt_cache_capacity_from_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(
            tmp.path().join("crap.toml"),
            "[database]\nstmt_cache_capacity = 64\n",
        )
        .unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.database.stmt_cache_capacity, 64);
    }
}
