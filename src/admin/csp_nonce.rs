//! Per-request Content-Security-Policy nonce plumbing.
//!
//! A fresh nonce is generated once per HTTP request, inserted into a
//! `tokio::task_local!` slot, and consumed by both the response's CSP header
//! (via the `security_headers` middleware) and the admin template context
//! (via `ContextBuilder`). Any inline `<script>` emitted by a built-in or
//! overlay template must carry `nonce="{{crap.csp_nonce}}"` to be allowed
//! by the browser.
//!
//! Using a task-local keeps handler signatures unchanged — no need to thread
//! the nonce through every `ContextBuilder::new` call. `try_with` returns
//! `Err` outside of a request scope (tests, startup paths), in which case
//! the empty string is substituted and inline scripts will be CSP-blocked
//! exactly as they would be without a matching nonce.
//!
//! The current value (22-char URL-safe nanoid, ~131 bits of entropy) is
//! well above the 128-bit minimum suggested by the W3C CSP3 spec.

use nanoid::nanoid;
use tokio::task_local;

/// A per-request Content-Security-Policy nonce.
#[derive(Clone, Debug)]
pub struct CspNonce(String);

impl CspNonce {
    /// Generate a fresh nonce for a new request.
    pub fn generate() -> Self {
        Self(nanoid!(22))
    }

    /// Raw nonce string — embed in CSP header and in `nonce="..."` attributes.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

task_local! {
    /// The active request's nonce. Set by the `security_headers` middleware
    /// via `CSP_NONCE.scope(...)` around the inner service.
    pub static CSP_NONCE: CspNonce;
}

/// Read the current task's nonce, or empty string if called outside a request
/// scope (tests, background tasks). Used by `ContextBuilder` to populate
/// `crap.csp_nonce` without threading the nonce through every handler.
pub fn current_nonce_or_empty() -> String {
    CSP_NONCE
        .try_with(|n| n.as_str().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_produces_nonempty_unique_values() {
        let a = CspNonce::generate();
        let b = CspNonce::generate();

        assert!(!a.as_str().is_empty());
        assert_ne!(a.as_str(), b.as_str());
    }

    #[test]
    fn generate_has_sufficient_entropy() {
        // 22-char nanoid alphabet is 64 symbols → 22 * 6 = 132 bits.
        let n = CspNonce::generate();
        assert_eq!(n.as_str().len(), 22);
    }

    #[tokio::test]
    async fn current_nonce_empty_outside_scope() {
        assert_eq!(current_nonce_or_empty(), "");
    }

    #[tokio::test]
    async fn current_nonce_visible_inside_scope() {
        let nonce = CspNonce::generate();
        let expected = nonce.as_str().to_string();

        let seen = CSP_NONCE
            .scope(nonce, async { current_nonce_or_empty() })
            .await;

        assert_eq!(seen, expected);
    }
}
