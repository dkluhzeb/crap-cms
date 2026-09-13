//! Per-key login rate limiter with sliding window.

use std::sync::Arc;

use tracing::{error, warn};

use super::{MemoryRateLimitBackend, SharedRateLimitBackend};

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
    fn prefixed_key(&self, key: &str) -> String {
        format!("{}:{}", self.prefix, key)
    }

    /// Check if a key is currently blocked (too many recent failures).
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

    /// Record a failed attempt for the given key.
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
    use super::*;

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
        std::thread::sleep(std::time::Duration::from_millis(10));
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

    /// Security: a backend failure must fail CLOSED (blocked), not silently
    /// disable brute-force protection for the duration of the outage.
    #[test]
    fn check_and_block_fails_closed_on_backend_error() {
        struct FailingBackend;
        impl super::super::RateLimitBackend for FailingBackend {
            fn count(&self, _key: &str, _window_secs: u64) -> anyhow::Result<u32> {
                anyhow::bail!("backend down")
            }
            fn record(&self, _key: &str, _window_secs: u64) -> anyhow::Result<()> {
                anyhow::bail!("backend down")
            }
            fn clear(&self, _key: &str) -> anyhow::Result<()> {
                anyhow::bail!("backend down")
            }
            fn check_and_record(
                &self,
                _key: &str,
                _max_count: u32,
                _window_secs: u64,
            ) -> anyhow::Result<bool> {
                anyhow::bail!("backend down")
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
        impl super::super::RateLimitBackend for FailingBackend {
            fn count(&self, _key: &str, _window_secs: u64) -> anyhow::Result<u32> {
                anyhow::bail!("backend down")
            }
            fn record(&self, _key: &str, _window_secs: u64) -> anyhow::Result<()> {
                anyhow::bail!("backend down")
            }
            fn clear(&self, _key: &str) -> anyhow::Result<()> {
                anyhow::bail!("backend down")
            }
            fn check_and_record(
                &self,
                _key: &str,
                _max_count: u32,
                _window_secs: u64,
            ) -> anyhow::Result<bool> {
                anyhow::bail!("backend down")
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
