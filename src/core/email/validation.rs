//! Email header-injection guard.
//!
//! Lettre (and generic HTTP webhook bodies) do NOT escape `\r` / `\n` embedded
//! in header-derived fields such as `subject`, `to`, `from`, `cc`, `bcc`, or
//! `reply_to`. A malicious Lua hook that can influence any of those fields
//! could splice in extra SMTP headers (e.g. `Bcc: attacker@evil.com`) or
//! entirely new emails.
//!
//! This module provides a single chokepoint — [`validate_no_crlf`] — that
//! must be called on every user-controllable header-ish string before the
//! value is handed to the email provider or stored in a queued job row.
//!
//! ### Scope
//!
//! - Header-derived fields (subject, to, from, cc, bcc, reply_to) MUST be
//!   validated. Any `\r`, `\n`, or raw NUL (`\0`) byte is rejected outright.
//! - Body fields (`text`, `html`) are intentionally NOT validated here:
//!   lettre MIME-encodes them as Content-Transfer-Encoded body bytes, and
//!   the webhook provider ships them as JSON string values which
//!   `serde_json` escapes. Adding a validator there would only produce
//!   false positives on legitimate multi-line content.

use anyhow::{Result, bail};

/// Reject a header-derived email field if it contains `\r`, `\n`, or `\0`.
///
/// Used for `subject`, `to`, `from`, `cc`, `bcc`, `reply_to` — anything that
/// ends up in an SMTP header or a webhook JSON header field. The error
/// message is deliberately generic so it can surface safely to callers.
pub fn validate_no_crlf(field_name: &str, value: &str) -> Result<()> {
    if value.bytes().any(|b| matches!(b, b'\r' | b'\n' | 0)) {
        bail!(
            "Email field '{field_name}' contains forbidden control characters — header injection rejected"
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_plain_string() {
        assert!(validate_no_crlf("subject", "Hello world").is_ok());
    }

    #[test]
    fn accepts_empty_string() {
        assert!(validate_no_crlf("subject", "").is_ok());
    }

    #[test]
    fn accepts_unicode() {
        assert!(validate_no_crlf("subject", "Héllo — wörld ✉").is_ok());
    }

    #[test]
    fn rejects_crlf() {
        let err = validate_no_crlf("subject", "Welcome\r\nBcc: x@y.z")
            .unwrap_err()
            .to_string();
        assert!(err.contains("header injection"), "unexpected error: {err}");
        assert!(err.contains("subject"), "field name missing: {err}");
    }

    #[test]
    fn rejects_bare_lf() {
        assert!(validate_no_crlf("subject", "line1\nline2").is_err());
    }

    #[test]
    fn rejects_bare_cr() {
        assert!(validate_no_crlf("subject", "line1\rline2").is_err());
    }

    #[test]
    fn rejects_null_byte() {
        assert!(validate_no_crlf("subject", "abc\0def").is_err());
    }

    #[test]
    fn error_names_the_field() {
        let err = validate_no_crlf("bcc", "attacker@evil.com\r\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("bcc"), "expected field name in error: {err}");
    }

    // ── Named regression tests per SEC-A (header injection). ──

    #[test]
    fn email_subject_rejects_crlf() {
        let err = validate_no_crlf("subject", "Welcome\r\nBcc: attacker@evil.com")
            .unwrap_err()
            .to_string();
        assert!(err.contains("header injection"), "unexpected: {err}");
        assert!(err.contains("subject"), "unexpected: {err}");
    }

    #[test]
    fn email_subject_rejects_bare_newline() {
        // Bare LF (no CR) must also be rejected — lettre does not sanitize either.
        assert!(validate_no_crlf("subject", "line1\nline2").is_err());
    }

    #[test]
    fn email_to_rejects_crlf() {
        let err = validate_no_crlf("to", "user@example.com\r\nBcc: x@y.z")
            .unwrap_err()
            .to_string();
        assert!(err.contains("header injection"), "unexpected: {err}");
        assert!(err.contains("to"), "unexpected: {err}");
    }

    #[test]
    fn email_from_rejects_crlf() {
        let err = validate_no_crlf("from", "me@example.com\r\nBcc: x@y.z")
            .unwrap_err()
            .to_string();
        assert!(err.contains("header injection"), "unexpected: {err}");
        assert!(err.contains("from"), "unexpected: {err}");
    }

    #[test]
    fn email_bcc_rejects_crlf() {
        let err = validate_no_crlf("bcc", "legit@example.com\r\nCc: smuggled@evil.com")
            .unwrap_err()
            .to_string();
        assert!(err.contains("header injection"), "unexpected: {err}");
        assert!(err.contains("bcc"), "unexpected: {err}");
    }
}
