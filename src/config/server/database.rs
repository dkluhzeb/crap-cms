//! `SQLite` / `PostgreSQL` backend, pool, and per-engine tuning settings.

use serde::{Deserialize, Serialize};

use crate::config::parsing::{serde_duration, serde_duration_ms};

/// `SQLite` database path and pool configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct DatabaseConfig {
    /// Database backend: `"sqlite"` (default) or `"postgres"`.
    #[serde(default = "default_backend")]
    pub backend: String,
    /// `PostgreSQL` connection URL (only used when `backend = "postgres"`).
    /// e.g., `"host=localhost user=crap dbname=crap_cms"`
    #[serde(default)]
    pub url: Option<String>,
    /// Path to the `SQLite` database file (only used when `backend = "sqlite"`).
    pub path: String,
    /// Maximum number of connections in the pool. Default: 64.
    pub pool_max_size: u32,
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

fn default_backend() -> String {
    "sqlite".to_string()
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            backend: default_backend(),
            url: None,
            path: "data/crap.db".to_string(),
            pool_max_size: 64,
            busy_timeout: 30000,
            connection_timeout: 30,
            cache_size: default_cache_size(),
            mmap_size: default_mmap_size(),
            wal_autocheckpoint: default_wal_autocheckpoint(),
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
}
