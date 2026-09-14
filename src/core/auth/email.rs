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
