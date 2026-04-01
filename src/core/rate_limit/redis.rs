//! Redis rate limit backend using sorted sets for sliding windows.
//!
//! Feature-gated behind `--features redis`.

use std::sync::Mutex;

use anyhow::{Context, Result};
use redis::{Commands, ConnectionLike};

use super::RateLimitBackend;

/// Redis-backed rate limiter using sorted sets for accurate sliding windows.
///
/// Each key is a sorted set where:
/// - Members are unique event IDs (nanosecond timestamps)
/// - Scores are Unix timestamps in milliseconds
///
/// Operations:
/// - `count`: ZREMRANGEBYSCORE (prune expired) + ZCARD
/// - `record`: ZADD with current timestamp + EXPIRE for cleanup
/// - `clear`: DEL
pub struct RedisRateLimitBackend {
    client: redis::Client,
    conn: Mutex<redis::Connection>,
    prefix: String,
}

impl RedisRateLimitBackend {
    pub fn new(url: &str, prefix: &str) -> Result<Self> {
        let client = redis::Client::open(url).context("Failed to create Redis client")?;

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
        })
    }

    fn prefixed_key(&self, key: &str) -> String {
        format!("{}{}", self.prefix, key)
    }

    fn with_conn<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&mut redis::Connection) -> Result<T>,
    {
        let mut guard = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Redis mutex poisoned: {}", e))?;

        match f(&mut guard) {
            Ok(val) => Ok(val),
            Err(first_err) => match self.client.get_connection() {
                Ok(new_conn) => {
                    *guard = new_conn;
                    f(&mut guard)
                }
                Err(_) => Err(first_err),
            },
        }
    }

    fn now_ms() -> f64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64()
            * 1000.0
    }
}

impl RateLimitBackend for RedisRateLimitBackend {
    fn count(&self, key: &str, window_secs: u64) -> Result<u32> {
        let pkey = self.prefixed_key(key);
        let now = Self::now_ms();
        let cutoff = now - (window_secs as f64 * 1000.0);

        self.with_conn(|conn| {
            redis::cmd("ZREMRANGEBYSCORE")
                .arg(&pkey)
                .arg("-inf")
                .arg(cutoff)
                .query::<()>(conn)
                .context("Redis ZREMRANGEBYSCORE failed")?;

            let count: u32 = conn.zcard(&pkey).context("Redis ZCARD failed")?;
            Ok(count)
        })
    }

    fn record(&self, key: &str, window_secs: u64) -> Result<()> {
        let pkey = self.prefixed_key(key);
        let now = Self::now_ms();
        let member = format!("{:.6}", now);
        let expire = (window_secs * 2).max(60) as i64;

        self.with_conn(|conn| {
            conn.zadd(&pkey, &member, now)
                .context("Redis ZADD failed")?;

            conn.expire(&pkey, expire).context("Redis EXPIRE failed")?;

            Ok(())
        })
    }

    fn check_and_record(&self, key: &str, max_count: u32, window_secs: u64) -> Result<bool> {
        let pkey = self.prefixed_key(key);
        let now = Self::now_ms();
        let cutoff = now - (window_secs as f64 * 1000.0);
        let member = format!("{:.6}", now);
        let expire = (window_secs * 2).max(60) as i64;

        // Atomic Lua script: prune expired, check count, conditionally add.
        // Runs as a single Redis command — no interleaving possible.
        let script = r#"
            redis.call('ZREMRANGEBYSCORE', KEYS[1], '-inf', ARGV[1])
            local count = redis.call('ZCARD', KEYS[1])
            if count >= tonumber(ARGV[2]) then
                return 0
            end
            redis.call('ZADD', KEYS[1], ARGV[3], ARGV[4])
            redis.call('EXPIRE', KEYS[1], ARGV[5])
            return 1
        "#;

        self.with_conn(|conn| {
            let result: i32 = redis::cmd("EVAL")
                .arg(script)
                .arg(1) // number of KEYS
                .arg(&pkey)
                .arg(cutoff)
                .arg(max_count)
                .arg(now)
                .arg(&member)
                .arg(expire)
                .query(conn)
                .context("Redis EVAL check_and_record failed")?;

            Ok(result == 1)
        })
    }

    fn clear(&self, key: &str) -> Result<()> {
        let pkey = self.prefixed_key(key);

        self.with_conn(|conn| {
            conn.del(&pkey).context("Redis DEL failed")?;
            Ok(())
        })
    }

    fn kind(&self) -> &'static str {
        "redis"
    }
}
