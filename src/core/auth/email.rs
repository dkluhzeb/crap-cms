//! Email normalization for auth.

use unicode_normalization::UnicodeNormalization;

/// The canonical form of an email address: trimmed of surrounding whitespace,
/// lowercased and NFC-composed. Email values are stored in this form, so an
/// address typed with different capitals or a decomposed accent is the same
/// address.
///
/// This is the single form used to key the per-email login / forgot-password
/// rate limiters across every surface (gRPC and admin), so an attacker can't
/// rotate casing or padding to get a fresh lockout bucket per spelling of one
/// account. `find_by_email` binds the same form.
#[must_use]
pub fn normalize_email(raw: &str) -> String {
    raw.trim().to_lowercase().nfc().collect()
}

/// The longest address SMTP can carry: RFC 5321's 256-octet path minus its
/// two angle brackets.
pub const MAX_EMAIL_OCTETS: usize = 254;

/// The canonical form of an email submitted to a pre-auth endpoint (login,
/// forgot-password, resend-verification), or `None` when it is longer than
/// any deliverable address.
///
/// Every such endpoint gates on this before touching a rate limiter or the
/// database, so an oversized "email" is refused for the cost of a length
/// check instead of being normalized, keyed and looked up.
#[must_use]
pub fn login_email_key(raw: &str) -> Option<String> {
    let trimmed = raw.trim();

    if trimmed.len() > MAX_EMAIL_OCTETS {
        return None;
    }

    Some(normalize_email(trimmed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_email_key_normalizes_a_deliverable_address() {
        assert_eq!(
            login_email_key("  Alice@Example.COM ").as_deref(),
            Some("alice@example.com")
        );
    }

    /// Regression: pre-auth endpoints normalized and rate-limited an email of
    /// any length, so one request could pin megabytes per limiter key.
    #[test]
    fn login_email_key_refuses_an_address_longer_than_smtp_allows() {
        let local = "a".repeat(MAX_EMAIL_OCTETS - "@x.io".len());
        let longest = format!("{local}@x.io");

        assert!(login_email_key(&longest).is_some());
        assert!(login_email_key(&format!("  {longest}  ")).is_some());
        assert!(login_email_key(&format!("a{longest}")).is_none());
        assert!(login_email_key(&"a".repeat(50 * 1024 * 1024)).is_none());
    }

    #[test]
    fn trims_and_lowercases() {
        assert_eq!(normalize_email("  Alice@Example.COM "), "alice@example.com");
    }

    #[test]
    fn idempotent_on_canonical_form() {
        let once = normalize_email("bob@host.dev");
        assert_eq!(normalize_email(&once), once);
    }

    /// A decomposed accent (`e` + U+0301) and its precomposed form are the same
    /// address, and non-ASCII capitals fold like ASCII ones.
    #[test]
    fn composes_accents_and_folds_non_ascii_case() {
        let composed = "ang\u{e8}le@j\u{fc}rgen.example";

        assert_eq!(
            normalize_email(" ANGE\u{300}LE@J\u{dc}RGEN.example "),
            composed
        );
        assert_eq!(
            normalize_email("Ange\u{300}le@ju\u{308}rgen.example"),
            composed
        );
        assert_eq!(normalize_email(composed), composed);
    }
}
