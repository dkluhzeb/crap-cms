//! Redis rate limit backend using sorted sets for sliding windows.
//!
//! Feature-gated behind `--features redis`.

use std::sync::Mutex;

use anyhow::{Context, Result};
use redis::Commands;

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
        F: Fn(&mut redis::Connection) -> Result<T>,
    {
        let mut guard = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Redis mutex poisoned: {e}"))?;

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

/// Convert a rate-limit `window_secs` value to f64 milliseconds for
/// score comparisons. Rate-limit windows are operator-configured in
/// seconds (typical values: 60..86400) — far below `2^53` where f64
/// integer precision starts to drift — so the cast is lossless in
/// practice. The lint is correct for arbitrary u64 inputs; we
/// localize the allow so a future caller that synthesizes the
/// argument differently doesn't silently inherit it.
#[allow(clippy::cast_precision_loss)]
fn window_secs_to_ms(window_secs: u64) -> f64 {
    window_secs as f64 * 1000.0
}

/// Compute the Redis `EXPIRE` ttl for a rate-limit window. Doubles
/// the window so an event recorded right at the boundary is still
/// counted on the next request, then clamps into `[MIN, MAX]`.
///
/// The upper cap matters: `EXPIRE` accepts `i64`, and an
/// unsaturated `u64::MAX` would happily wrap or — with a naive
/// `unwrap_or(i64::MAX)` fallback — turn into a Redis key that
/// essentially never expires, leaking memory if an operator
/// misconfigures the window or a future caller plumbs in a bogus
/// value. One week is well above any sensible rate-limit window
/// (typical: 60s to 24h) and well below `i64::MAX`, so the cast
/// after clamping is infallible.
fn ttl_secs(window_secs: u64) -> i64 {
    // 60s: ensure the Redis key survives at least one window even
    // for sub-minute windows (Redis EXPIRE rounds aggressively).
    // 7 days: bounded ceiling so a buggy `window_secs` value can't
    // create an effectively-permanent key.
    const MIN_TTL_SECS: u64 = 60;
    const MAX_TTL_SECS: u64 = 7 * 24 * 60 * 60;
    let ttl = window_secs
        .saturating_mul(2)
        .clamp(MIN_TTL_SECS, MAX_TTL_SECS);
    // ttl ∈ [MIN_TTL_SECS, MAX_TTL_SECS] ⊂ i64.
    i64::try_from(ttl).expect("ttl clamped within i64 range")
}

impl RateLimitBackend for RedisRateLimitBackend {
    fn count(&self, key: &str, window_secs: u64) -> Result<u32> {
        let pkey = self.prefixed_key(key);
        let now = Self::now_ms();
        let cutoff = now - window_secs_to_ms(window_secs);

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
        let member = format!("{now:.6}");
        let expire = ttl_secs(window_secs);

        self.with_conn(|conn| {
            conn.zadd::<_, _, _, ()>(&pkey, &member, now)
                .context("Redis ZADD failed")?;

            conn.expire::<_, ()>(&pkey, expire)
                .context("Redis EXPIRE failed")?;

            Ok(())
        })
    }

    fn check_and_record(&self, key: &str, max_count: u32, window_secs: u64) -> Result<bool> {
        let pkey = self.prefixed_key(key);
        let now = Self::now_ms();
        let cutoff = now - window_secs_to_ms(window_secs);
        let member = format!("{now:.6}");
        let expire = ttl_secs(window_secs);

        // Atomic Lua script: prune expired, check count, conditionally add.
        // Runs as a single Redis command — no interleaving possible.
        let script = r"
            redis.call('ZREMRANGEBYSCORE', KEYS[1], '-inf', ARGV[1])
            local count = redis.call('ZCARD', KEYS[1])
            if count >= tonumber(ARGV[2]) then
                return 0
            end
            redis.call('ZADD', KEYS[1], ARGV[3], ARGV[4])
            redis.call('EXPIRE', KEYS[1], ARGV[5])
            return 1
        ";

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
            conn.del::<_, ()>(&pkey).context("Redis DEL failed")?;
            Ok(())
        })
    }

    fn refund(&self, key: &str, _window_secs: u64) -> Result<()> {
        let pkey = self.prefixed_key(key);

        self.with_conn(|conn| {
            // Drop the single highest-scored (most-recent) member of the sorted
            // set — the event this attempt added — leaving older events intact.
            redis::cmd("ZREMRANGEBYRANK")
                .arg(&pkey)
                .arg(-1)
                .arg(-1)
                .query::<()>(conn)
                .context("Redis ZREMRANGEBYRANK failed")?;

            Ok(())
        })
    }

    fn kind(&self) -> &'static str {
        "redis"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ttl_secs_clamps_zero_to_minimum() {
        // A zero-window config (probably a bug) still produces a
        // sane non-zero TTL so we don't churn the Redis key.
        assert_eq!(ttl_secs(0), 60);
    }

    #[test]
    fn ttl_secs_doubles_normal_window() {
        assert_eq!(ttl_secs(300), 600);
        assert_eq!(ttl_secs(3600), 7200);
    }

    #[test]
    fn ttl_secs_caps_huge_window_at_one_week() {
        // A misconfigured huge window must not produce an
        // effectively-permanent Redis key — the cap matters
        // because Redis EXPIRE accepts i64 (a wrap or
        // i64::MAX fallback would leak memory).
        const ONE_WEEK: i64 = 7 * 24 * 60 * 60;
        assert_eq!(ttl_secs(u64::MAX), ONE_WEEK);
        assert_eq!(ttl_secs(u64::MAX / 2), ONE_WEEK);
        // A window one week long (would naturally double to two)
        // still caps at the ceiling.
        assert_eq!(ttl_secs(7 * 24 * 60 * 60), ONE_WEEK);
    }

    #[test]
    fn window_secs_to_ms_is_correct_for_typical_values() {
        assert!((window_secs_to_ms(1) - 1000.0).abs() < f64::EPSILON);
        assert!((window_secs_to_ms(60) - 60_000.0).abs() < f64::EPSILON);
    }
}
