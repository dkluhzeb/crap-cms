//! Redis cache backend.
//!
//! Uses synchronous Redis operations via the `redis` crate.
//! Feature-gated behind `--features redis`.

use std::sync::Mutex;

use anyhow::{Context, Result, anyhow};
use redis::{Client, Commands};

use crate::core::cache::CacheBackend;

/// Redis-backed cache with key prefixing and connection reuse.
///
/// All keys are prefixed with `prefix` to namespace them within a shared Redis
/// instance (e.g., `crap:populate:posts:123:en`).
///
/// Holds a single reusable connection behind a `Mutex`. If the connection
/// breaks (network error, Redis restart), it is automatically replaced on
/// the next operation.
pub struct RedisCache {
    client: Client,
    conn: Mutex<redis::Connection>,
    prefix: String,
    ttl_secs: u64,
}

impl RedisCache {
    /// Create a new Redis cache backend.
    ///
    /// `ttl_secs` controls per-key expiry: `0` = no expiry (keys live until
    /// explicit `clear()`), `> 0` = each key expires after this many seconds.
    ///
    /// Validates the connection at creation time — returns an error if Redis
    /// is unreachable.
    pub fn new(url: &str, prefix: &str, ttl_secs: u64) -> Result<Self> {
        let client = Client::open(url).context("Failed to create Redis client")?;

        let mut conn = client
            .get_connection()
            .context("Failed to connect to Redis")?;

        redis::cmd("PING")
            .query::<String>(&mut conn)
            .context("Redis PING failed")?;

        Ok(Self {
            client,
            conn: Mutex::new(conn),
            prefix: prefix.to_string(),
            ttl_secs,
        })
    }

    /// Build the full Redis key with prefix.
    fn prefixed_key(&self, key: &str) -> String {
        format!("{}{}", self.prefix, key)
    }

    /// Get the shared connection, reconnecting if it's broken.
    fn with_conn<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&mut redis::Connection) -> Result<T>,
    {
        let mut guard = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Redis mutex poisoned: {}", e))?;

        // Try the operation first
        match f(&mut guard) {
            Ok(val) => Ok(val),
            Err(first_err) => {
                // Connection may be broken — try to reconnect once
                match self.client.get_connection() {
                    Ok(new_conn) => {
                        *guard = new_conn;
                        f(&mut guard)
                    }
                    Err(_) => Err(first_err),
                }
            }
        }
    }
}

impl CacheBackend for RedisCache {
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let pkey = self.prefixed_key(key);

        self.with_conn(|conn| {
            let result: Option<Vec<u8>> = conn.get(&pkey).context("Redis GET failed")?;

            Ok(result)
        })
    }

    fn set(&self, key: &str, value: &[u8]) -> Result<()> {
        let pkey = self.prefixed_key(key);
        let ttl = self.ttl_secs;

        self.with_conn(|conn| {
            if ttl > 0 {
                redis::cmd("SETEX")
                    .arg(&pkey)
                    .arg(ttl)
                    .arg(value)
                    .query::<()>(conn)
                    .context("Redis SETEX failed")?;
            } else {
                conn.set(&pkey, value).context("Redis SET failed")?;
            }

            Ok(())
        })
    }

    fn delete(&self, key: &str) -> Result<()> {
        let pkey = self.prefixed_key(key);

        self.with_conn(|conn| {
            conn.del(&pkey).context("Redis DEL failed")?;

            Ok(())
        })
    }

    fn clear(&self) -> Result<()> {
        let pattern = format!("{}*", self.prefix);

        // SCAN + DEL in batches. Not atomic — keys written between iterations
        // may survive. Acceptable for cache invalidation: survivors are cleared
        // on the next periodic cycle or write-triggered clear.
        self.with_conn(|conn| {
            let mut cursor = 0u64;

            loop {
                let (next_cursor, keys): (u64, Vec<String>) = redis::cmd("SCAN")
                    .arg(cursor)
                    .arg("MATCH")
                    .arg(&pattern)
                    .arg("COUNT")
                    .arg(1000)
                    .query(conn)
                    .context("Redis SCAN failed")?;

                if !keys.is_empty() {
                    redis::cmd("DEL")
                        .arg(&keys)
                        .query::<()>(conn)
                        .context("Redis DEL failed during clear")?;
                }

                cursor = next_cursor;

                if cursor == 0 {
                    break;
                }
            }

            Ok(())
        })
    }

    fn has(&self, key: &str) -> Result<bool> {
        let pkey = self.prefixed_key(key);

        self.with_conn(|conn| {
            let exists: bool = conn.exists(&pkey).context("Redis EXISTS failed")?;

            Ok(exists)
        })
    }

    fn kind(&self) -> &'static str {
        "redis"
    }
}
