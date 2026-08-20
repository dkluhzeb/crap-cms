//! Email normalization for auth.

/// The canonical normal form of an email address for auth comparisons: trimmed
/// of surrounding whitespace and lowercased.
///
/// This is the single form used to key the per-email login / forgot-password
/// rate limiters across every surface (gRPC and admin), so an attacker can't
/// rotate casing or padding to get a fresh lockout bucket per spelling of one
/// account. It matches the case-insensitive `find_by_email` lookup
/// (`LOWER(email) = LOWER(?)`) and the trimmed form stored at the write edge.
#[must_use]
pub fn normalize_email(raw: &str) -> String {
    raw.trim().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_and_lowercases() {
        assert_eq!(normalize_email("  Alice@Example.COM "), "alice@example.com");
    }

    #[test]
    fn idempotent_on_canonical_form() {
        let once = normalize_email("bob@host.dev");
        assert_eq!(normalize_email(&once), once);
    }
}
