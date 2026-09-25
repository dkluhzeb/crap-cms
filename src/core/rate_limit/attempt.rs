//! One pre-auth attempt counted against a per-IP and a per-identity budget.

use super::LoginRateLimiter;
use crate::core::ClientIp;

/// A per-IP budget paired with a per-identity budget (an email, a user id),
/// the shape every credential-bearing auth endpoint throttles with.
///
/// The IP budget is consulted FIRST, and an attempt from an IP that is already
/// over its budget never touches the identity budget. Otherwise a blocked IP
/// could still grow the identity limiter by one fresh key per request — one
/// per invented email — for as long as it keeps sending.
pub struct AttemptBudget<'a> {
    ip: &'a LoginRateLimiter,
    identity: &'a LoginRateLimiter,
}

impl<'a> AttemptBudget<'a> {
    /// Pair a per-IP limiter with a per-identity limiter.
    #[must_use]
    pub fn new(ip: &'a LoginRateLimiter, identity: &'a LoginRateLimiter) -> Self {
        Self { ip, identity }
    }

    /// Atomically record one attempt and report whether it is BLOCKED.
    ///
    /// The IP budget records first; only an attempt it admits is recorded
    /// against `identity`. Fails closed like the underlying limiters.
    #[must_use]
    pub fn check_and_block(&self, client: &ClientIp, identity: &str) -> bool {
        if self.ip.check_and_block_ip(client) {
            return true;
        }

        self.identity.check_and_block(identity)
    }

    /// Settle a successful attempt: the identity proved itself, so its budget
    /// is cleared; the shared IP budget only gets this one attempt refunded,
    /// so a success never wipes other identities' failures from the same IP.
    pub fn settle_success(&self, client: &ClientIp, identity: &str) {
        self.identity.clear(identity);
        self.ip.refund_ip(client);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::core::rate_limit::{MemoryRateLimitBackend, RateLimitBackend};

    fn client(ip: &str) -> ClientIp {
        ClientIp::new(ip.parse().unwrap())
    }

    /// Regression: both budgets were recorded on every attempt, so an IP over
    /// its budget still inserted one fresh identity key per request.
    #[test]
    fn a_blocked_ip_records_nothing_against_the_identity() {
        let backend: Arc<MemoryRateLimitBackend> = Arc::new(MemoryRateLimitBackend::new());
        let ip = LoginRateLimiter::with_backend(backend.clone(), "ip", 1, 60);
        let email = LoginRateLimiter::with_backend(backend.clone(), "email", 5, 60);
        let budget = AttemptBudget::new(&ip, &email);
        let from = client("203.0.113.5");

        assert!(!budget.check_and_block(&from, "a@example.com"));
        assert!(budget.check_and_block(&from, "b@example.com"));
        assert!(budget.check_and_block(&from, "c@example.com"));

        assert!(!email.is_blocked("b@example.com"));
        assert_eq!(backend.key_count(), 2, "one IP key and one email key");
    }

    #[test]
    fn an_exhausted_identity_blocks_while_the_ip_still_counts() {
        let backend: Arc<dyn RateLimitBackend> = Arc::new(MemoryRateLimitBackend::new());
        let ip = LoginRateLimiter::with_backend(backend.clone(), "ip", 3, 60);
        let email = LoginRateLimiter::with_backend(backend, "email", 1, 60);
        let budget = AttemptBudget::new(&ip, &email);
        let from = client("203.0.113.5");

        assert!(!budget.check_and_block(&from, "a@example.com"));
        assert!(budget.check_and_block(&from, "a@example.com"));
        assert!(!budget.check_and_block(&from, "b@example.com"));
        assert!(
            budget.check_and_block(&from, "c@example.com"),
            "IP budget spent"
        );
    }

    #[test]
    fn a_success_clears_the_identity_and_refunds_one_ip_attempt() {
        let backend: Arc<dyn RateLimitBackend> = Arc::new(MemoryRateLimitBackend::new());
        let ip = LoginRateLimiter::with_backend(backend.clone(), "ip", 2, 60);
        let email = LoginRateLimiter::with_backend(backend, "email", 1, 60);
        let budget = AttemptBudget::new(&ip, &email);
        let from = client("203.0.113.5");

        assert!(!budget.check_and_block(&from, "a@example.com"));
        budget.settle_success(&from, "a@example.com");

        assert!(!budget.check_and_block(&from, "a@example.com"));
        assert!(!budget.check_and_block(&from, "b@example.com"));
        assert!(budget.check_and_block(&from, "c@example.com"));
    }
}
