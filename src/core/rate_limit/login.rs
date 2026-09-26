//! Per-key login rate limiter with sliding window.

use std::sync::Arc;

use sha2::{Digest, Sha256};
use tracing::{error, warn};

use super::{MemoryRateLimitBackend, SharedRateLimitBackend};
use crate::core::{ClientIp, hex::hex_encode};

/// Per-key login rate limiter. Tracks failed attempts in a sliding window
/// and blocks further attempts after a configurable threshold.
///
/// Thread-safe — the backend handles synchronization internally.
pub struct LoginRateLimiter {
    backend: SharedRateLimitBackend,
    prefix: String,
    max_attempts: u32,
    window_secs: u64,
}

/// Keyspace for the per-email resend-verification budget.
///
/// Both the admin route and the gRPC handler derive their limiter with
/// [`LoginRateLimiter::rescoped`] from the forgot-password limiter under this
/// one name, so the two surfaces share thresholds by construction and never a
/// budget with the flow they were split from.
pub const RESEND_VERIFICATION_KEYSPACE: &str = "resend_verification";

/// Keyspace for the per-IP resend-verification budget. See
/// [`RESEND_VERIFICATION_KEYSPACE`].
pub const IP_RESEND_VERIFICATION_KEYSPACE: &str = "ip_resend_verification";

/// Keyspace for per-IP password-reset TOKEN attempts. Separate from the
/// forgot-password request budget so the two flows cannot drain each other,
/// and shared by every surface so an attacker cannot switch surfaces for a
/// fresh budget.
pub const IP_RESET_PASSWORD_KEYSPACE: &str = "ip_reset_password";

/// Keyspace for per-IP email-verification TOKEN attempts, shared by every
/// surface. Separate from the forgot-password budget so a burst of
/// verification attempts cannot exhaust what a password reset from the same IP
/// needs.
pub const IP_VERIFY_EMAIL_KEYSPACE: &str = "ip_verify_email";

/// Keyspace for per-IP failed API-key attempts on the MCP HTTP endpoint. Sized
/// like the per-IP login budget but separate from it, so MCP failures and
/// admin logins from one address never drain each other.
pub const IP_MCP_API_KEY_KEYSPACE: &str = "ip_mcp_api_key";

/// Keyspace for per-user MFA code ISSUANCE (email/custom delivery). Shared by
/// every surface: the login limiter cannot cap issuance because a successful
/// password clears it.
pub const MFA_ISSUE_KEYSPACE: &str = "mfa_issue";

impl LoginRateLimiter {
    /// Create a rate limiter with an explicit backend and prefix.
    ///
    /// `prefix` distinguishes different limiters sharing the same backend
    /// (e.g., `"login"`, `"ip_login"`, `"forgot"`, `"ip_forgot"`).
    pub fn with_backend(
        backend: SharedRateLimitBackend,
        prefix: &str,
        max_attempts: u32,
        window_seconds: u64,
    ) -> Self {
        Self {
            backend,
            prefix: prefix.to_string(),
            max_attempts,
            window_secs: window_seconds,
        }
    }

    /// Create a rate limiter with the default in-memory backend.
    ///
    /// Convenience constructor for single-server deployments and tests.
    #[must_use]
    pub fn new(max_attempts: u32, window_seconds: u64) -> Self {
        Self::with_backend(
            Arc::new(MemoryRateLimitBackend::new()),
            "",
            max_attempts,
            window_seconds,
        )
    }

    /// The same thresholds and backing store, under a different keyspace.
    ///
    /// Lets a second endpoint reuse a configured budget's *size* without
    /// sharing the budget itself, so a burst on one cannot lock a caller out
    /// of the other. Cheap — an `Arc` clone and two copies.
    #[must_use]
    pub fn rescoped(&self, prefix: &str) -> Self {
        Self::with_backend(self.backend(), prefix, self.max_attempts, self.window_secs)
    }

    /// The shared backend this limiter records into. Lets other limiters
    /// (e.g. per-route rate limits) reuse the same backing store — and thus the
    /// same cross-instance state when a Redis backend is configured — instead of
    /// each spinning up an isolated in-memory store.
    #[must_use]
    pub fn backend(&self) -> SharedRateLimitBackend {
        self.backend.clone()
    }

    /// Build the prefixed key for backend storage.
    ///
    /// The caller's key is stored as its SHA-256 digest, never verbatim: keys
    /// are caller-controlled (an email typed into a pre-auth form, a user id,
    /// an address), and a verbatim key lets one request pin arbitrarily many
    /// bytes in the backend for the whole window. Hashing here covers every
    /// limiter and every backend at once.
    fn prefixed_key(&self, key: &str) -> String {
        format!(
            "{}:{}",
            self.prefix,
            hex_encode(&Sha256::digest(key.as_bytes())[..])
        )
    }

    /// Check if a key is currently blocked (too many recent failures).
    ///
    /// A read-only probe (for tests and diagnostics): an attempt must be gated
    /// by [`Self::check_and_block`], which checks and counts in one step.
    ///
    /// Fails CLOSED: when the backend can't be reached (e.g. a Redis outage),
    /// the key is treated as blocked. Failing open here would silently
    /// disable brute-force protection for exactly as long as the outage —
    /// an attacker-friendly window. The error is logged so operators can
    /// tell infrastructure trouble from real lockouts.
    #[must_use]
    pub fn is_blocked(&self, key: &str) -> bool {
        let pkey = self.prefixed_key(key);

        match self.backend.count(&pkey, self.window_secs) {
            Ok(count) => count >= self.max_attempts,
            Err(e) => {
                error!("Rate limit backend unavailable — failing closed: {e:#}");
                true
            }
        }
    }

    /// Atomically record an attempt and report whether it is BLOCKED.
    ///
    /// Performs the limit check and the increment as a single backend
    /// operation, closing the check-then-record race that `is_blocked` +
    /// `record_failure` leaves open: under that split, a burst of concurrent
    /// requests can all observe an under-limit count before any of them
    /// records, letting more than `max_attempts` through per window. Call this
    /// once at the start of an attempt instead.
    ///
    /// Returns `true` when the key is at/over the limit (the attempt should be
    /// rejected); the at-limit call does not increment further, so the count
    /// stays bounded. On a successful operation, call [`Self::clear`] to wipe
    /// the recorded attempts.
    ///
    /// Fails CLOSED on backend error (treats the attempt as blocked), matching
    /// [`Self::is_blocked`].
    #[must_use]
    pub fn check_and_block(&self, key: &str) -> bool {
        let pkey = self.prefixed_key(key);

        match self
            .backend
            .check_and_record(&pkey, self.max_attempts, self.window_secs)
        {
            Ok(allowed) => !allowed,
            Err(e) => {
                error!("Rate limit backend unavailable — failing closed: {e:#}");
                true
            }
        }
    }

    /// [`Self::check_and_block`] for a per-IP limiter: the attempt counts
    /// against the client's rate-limit bucket ([`ClientIp::rate_limit_key`]),
    /// never its exact address.
    #[must_use]
    pub fn check_and_block_ip(&self, client: &ClientIp) -> bool {
        self.check_and_block(&client.rate_limit_key())
    }

    /// [`Self::refund`] for a per-IP limiter, keyed like
    /// [`Self::check_and_block_ip`].
    pub fn refund_ip(&self, client: &ClientIp) {
        self.refund(&client.rate_limit_key());
    }

    /// Record a failed attempt for the given key.
    ///
    /// Test-only: a production attempt is counted by [`Self::check_and_block`]
    /// in the same step as its check — a separate record reopens the race.
    #[cfg(test)]
    pub fn record_failure(&self, key: &str) {
        let pkey = self.prefixed_key(key);

        if let Err(e) = self.backend.record(&pkey, self.window_secs) {
            warn!("Rate limit record failed: {:#}", e);
        }
    }

    /// Clear all failed attempts for the given key (e.g., on successful login).
    pub fn clear(&self, key: &str) {
        let pkey = self.prefixed_key(key);

        if let Err(e) = self.backend.clear(&pkey) {
            warn!("Rate limit clear failed: {:#}", e);
        }
    }

    /// Refund a single recorded attempt for `key` (undo one increment). Unlike
    /// [`Self::clear`], this leaves other events intact — use it on a *shared*
    /// limiter (e.g. per-IP) when an attempt turned out legitimate, so a
    /// success doesn't wipe unrelated suspicious attempts from the same IP.
    pub fn refund(&self, key: &str) {
        let pkey = self.prefixed_key(key);

        if let Err(e) = self.backend.refund(&pkey, self.window_secs) {
            warn!("Rate limit refund failed: {:#}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Mutex, thread::sleep, time::Duration};

    use anyhow::{Result as AnyResult, bail};

    use super::*;
    use crate::core::rate_limit::RateLimitBackend;

    fn memory_backend() -> SharedRateLimitBackend {
        Arc::new(MemoryRateLimitBackend::new())
    }

    #[test]
    fn is_blocked_initially_false() {
        let limiter = LoginRateLimiter::with_backend(memory_backend(), "test", 3, 60);
        assert!(!limiter.is_blocked("test@example.com"));
    }

    #[test]
    fn blocks_after_max_attempts() {
        let limiter = LoginRateLimiter::with_backend(memory_backend(), "test", 3, 60);
        limiter.record_failure("test@example.com");
        limiter.record_failure("test@example.com");
        assert!(!limiter.is_blocked("test@example.com"));
        limiter.record_failure("test@example.com");
        assert!(limiter.is_blocked("test@example.com"));
    }

    #[test]
    fn clear_resets_attempts() {
        let limiter = LoginRateLimiter::with_backend(memory_backend(), "test", 2, 60);
        limiter.record_failure("a@b.com");
        limiter.record_failure("a@b.com");
        assert!(limiter.is_blocked("a@b.com"));
        limiter.clear("a@b.com");
        assert!(!limiter.is_blocked("a@b.com"));
    }

    #[test]
    fn different_emails_independent() {
        let limiter = LoginRateLimiter::with_backend(memory_backend(), "test", 2, 60);
        limiter.record_failure("a@b.com");
        limiter.record_failure("a@b.com");
        assert!(limiter.is_blocked("a@b.com"));
        assert!(!limiter.is_blocked("c@d.com"));
    }

    #[test]
    fn expired_attempts_are_pruned() {
        let limiter = LoginRateLimiter::with_backend(memory_backend(), "test", 2, 0);
        limiter.record_failure("a@b.com");
        limiter.record_failure("a@b.com");
        sleep(Duration::from_millis(10));
        assert!(!limiter.is_blocked("a@b.com"));
    }

    #[test]
    fn prefix_isolates_limiters() {
        let backend = memory_backend();
        let login = LoginRateLimiter::with_backend(backend.clone(), "login", 2, 60);
        let forgot = LoginRateLimiter::with_backend(backend, "forgot", 2, 60);

        login.record_failure("a@b.com");
        login.record_failure("a@b.com");
        assert!(login.is_blocked("a@b.com"));
        assert!(!forgot.is_blocked("a@b.com"));
    }

    /// `rescoped` keeps a budget's size but not the budget: a limiter derived
    /// from a base shares its threshold and window, yet exhausting it never
    /// blocks the base. Both resend-verification surfaces derive their
    /// limiters this way, so their thresholds cannot drift apart again.
    #[test]
    fn rescoped_shares_thresholds_but_not_counters() {
        let base = LoginRateLimiter::with_backend(memory_backend(), "forgot", 2, 60);
        let resend = base.rescoped(RESEND_VERIFICATION_KEYSPACE);

        assert!(!resend.check_and_block("a@b.com"));
        assert!(!resend.check_and_block("a@b.com"));
        assert!(
            resend.check_and_block("a@b.com"),
            "the inherited threshold of 2 applies"
        );

        assert!(!base.is_blocked("a@b.com"), "the base budget is untouched");
    }

    #[test]
    fn check_and_block_records_and_blocks_at_limit() {
        let limiter = LoginRateLimiter::with_backend(memory_backend(), "test", 3, 60);

        // Each call records; under the limit it does not block.
        assert!(!limiter.check_and_block("a@b.com"));
        assert!(!limiter.check_and_block("a@b.com"));
        assert!(!limiter.check_and_block("a@b.com"));

        // At the limit: blocked, and the count stays bounded (no over-count).
        assert!(limiter.check_and_block("a@b.com"));
        assert!(limiter.check_and_block("a@b.com"));

        // A success clears, re-opening the window.
        limiter.clear("a@b.com");
        assert!(!limiter.check_and_block("a@b.com"));
    }

    /// A refund removes only ONE (the most recent) recorded attempt, unlike
    /// `clear` which wipes all. This is what stops a success on a shared per-IP
    /// limiter from erasing other accounts' failures from the same IP.
    #[test]
    fn refund_removes_only_the_latest_attempt() {
        let limiter = LoginRateLimiter::with_backend(memory_backend(), "ip", 3, 60);

        // Two failures accumulate toward the IP limit (max 3, so not yet blocked).
        limiter.record_failure("1.2.3.4");
        limiter.record_failure("1.2.3.4");
        assert!(!limiter.is_blocked("1.2.3.4"));

        // A third attempt (a success) records, then is refunded — net zero.
        assert!(!limiter.check_and_block("1.2.3.4"));
        limiter.refund("1.2.3.4");

        // The two earlier failures remain (refund didn't wipe them like clear).
        limiter.record_failure("1.2.3.4");
        assert!(
            limiter.is_blocked("1.2.3.4"),
            "the two prior failures plus one more must reach the limit",
        );
    }

    /// Records every key the limiter hands its backend.
    #[derive(Default)]
    struct KeyLog(Mutex<Vec<String>>);

    impl RateLimitBackend for KeyLog {
        fn count(&self, key: &str, _window_secs: u64) -> AnyResult<u32> {
            self.0.lock().unwrap().push(key.to_string());
            Ok(0)
        }
        fn record(&self, key: &str, _window_secs: u64) -> AnyResult<()> {
            self.0.lock().unwrap().push(key.to_string());
            Ok(())
        }
        fn clear(&self, key: &str) -> AnyResult<()> {
            self.0.lock().unwrap().push(key.to_string());
            Ok(())
        }
        fn check_and_record(
            &self,
            key: &str,
            _max_count: u32,
            _window_secs: u64,
        ) -> AnyResult<bool> {
            self.0.lock().unwrap().push(key.to_string());
            Ok(true)
        }
        fn kind(&self) -> &'static str {
            "key-log"
        }
    }

    /// Regression: the caller's key was stored verbatim, so one pre-auth
    /// request carrying a multi-megabyte "email" pinned that many bytes in the
    /// backend for the whole window. Every stored key is now a fixed-size
    /// digest, whatever the caller passes.
    #[test]
    fn stored_keys_are_fixed_size_whatever_the_input() {
        let log = Arc::new(KeyLog::default());
        let limiter = LoginRateLimiter::with_backend(log.clone(), "login", 5, 60);
        let huge = "a".repeat(1024 * 1024);

        assert!(!limiter.check_and_block(&huge));
        assert!(!limiter.check_and_block("a@b.com"));
        limiter.clear(&huge);

        let keys = log.0.lock().unwrap().clone();
        assert_eq!(keys.len(), 3);
        assert!(keys.iter().all(|k| k.len() == "login:".len() + 64));
        assert_eq!(keys[0], keys[2], "the same input maps to the same key");
        assert_ne!(keys[0], keys[1]);
    }

    /// Per-IP helpers key on the client's rate-limit bucket, so two addresses
    /// inside one IPv6 /64 share a budget.
    #[test]
    fn ip_helpers_key_on_the_rate_limit_bucket() {
        let limiter = LoginRateLimiter::with_backend(memory_backend(), "ip", 1, 60);
        let a = ClientIp::new("2001:db8:1:2::1".parse().unwrap());
        let b = ClientIp::new("2001:db8:1:2::2".parse().unwrap());

        assert!(!limiter.check_and_block_ip(&a));
        assert!(limiter.check_and_block_ip(&b), "same /64, same budget");

        limiter.refund_ip(&a);
        assert!(!limiter.check_and_block_ip(&b));
    }

    /// Security: a backend failure must fail CLOSED (blocked), not silently
    /// disable brute-force protection for the duration of the outage.
    #[test]
    fn check_and_block_fails_closed_on_backend_error() {
        struct FailingBackend;
        impl RateLimitBackend for FailingBackend {
            fn count(&self, _key: &str, _window_secs: u64) -> AnyResult<u32> {
                bail!("backend down")
            }
            fn record(&self, _key: &str, _window_secs: u64) -> AnyResult<()> {
                bail!("backend down")
            }
            fn clear(&self, _key: &str) -> AnyResult<()> {
                bail!("backend down")
            }
            fn check_and_record(
                &self,
                _key: &str,
                _max_count: u32,
                _window_secs: u64,
            ) -> AnyResult<bool> {
                bail!("backend down")
            }
            fn kind(&self) -> &'static str {
                "failing"
            }
        }

        let limiter = LoginRateLimiter::with_backend(Arc::new(FailingBackend), "t", 5, 60);
        assert!(
            limiter.check_and_block("user@example.com"),
            "backend failure must block (fail closed)"
        );
    }

    /// Security: a backend failure must fail CLOSED (blocked), not silently
    /// disable brute-force protection for the duration of the outage.
    #[test]
    fn backend_error_fails_closed() {
        struct FailingBackend;
        impl RateLimitBackend for FailingBackend {
            fn count(&self, _key: &str, _window_secs: u64) -> AnyResult<u32> {
                bail!("backend down")
            }
            fn record(&self, _key: &str, _window_secs: u64) -> AnyResult<()> {
                bail!("backend down")
            }
            fn clear(&self, _key: &str) -> AnyResult<()> {
                bail!("backend down")
            }
            fn check_and_record(
                &self,
                _key: &str,
                _max_count: u32,
                _window_secs: u64,
            ) -> AnyResult<bool> {
                bail!("backend down")
            }
            fn kind(&self) -> &'static str {
                "failing"
            }
        }

        let limiter = LoginRateLimiter::with_backend(Arc::new(FailingBackend), "t", 5, 60);
        assert!(
            limiter.is_blocked("user@example.com"),
            "backend failure must block (fail closed)"
        );
    }
}
