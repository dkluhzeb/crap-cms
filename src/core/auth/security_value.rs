//! One-way hashing for the short-lived security values stored on a user row:
//! password-reset tokens, email-verification tokens, and MFA codes.
//!
//! These are bearer credentials — whoever presents the value is treated as the
//! account owner for the length of its window. Storing them verbatim means a
//! read of the table (or of a backup) inside that window is an account
//! takeover with no further work. Storing a digest means the stored form is
//! not the credential.
//!
//! Two functions, because the two kinds of value have very different entropy:
//!
//! - **Tokens** (reset, verification) are 32-character nanoids. A plain
//!   SHA-256 is enough: there is no preimage to search. Not a password hash,
//!   deliberately — there is nothing to slow an attacker down against, and the
//!   verification path runs on every request carrying a token.
//! - **MFA codes** are six decimal digits. A plain digest of one is *not* a
//!   one-way store: the whole preimage space is 10^6, so a table over it
//!   inverts any stored digest instantly. The code is therefore keyed with the
//!   auth secret, which a database read alone does not yield. Rotating
//!   `[auth] secret` invalidates outstanding codes, which is harmless for a
//!   five-minute value.

use ring::{digest, hmac};
use subtle::ConstantTimeEq;

use crate::core::hex::hex_encode;

/// Domain separator, so an MFA digest can never collide with another HMAC
/// this codebase computes under the same secret.
const MFA_CONTEXT: &str = "crap-cms:mfa-code:v1";

/// Hash a high-entropy security token for storage and lookup, as lowercase
/// hex.
///
/// The caller keeps handling the raw value (it goes in the email); only the
/// stored and compared form is the digest.
#[must_use]
pub fn hash_security_value(value: &str) -> String {
    hex_encode(digest::digest(&digest::SHA256, value.as_bytes()).as_ref())
}

/// Whether a presented token matches a stored digest, in constant time.
///
/// Both digests are the same length, so the comparison leaks nothing about
/// how far a wrong value matched.
#[must_use]
pub fn security_value_matches(presented: &str, stored_hash: &str) -> bool {
    let presented_hash = hash_security_value(presented);

    bool::from(presented_hash.as_bytes().ct_eq(stored_hash.as_bytes()))
}

/// Hash a low-entropy MFA code under the auth secret, as lowercase hex.
///
/// Keyed rather than bare: six digits is a 10^6 preimage space, so an
/// unkeyed digest of one is invertible by table lookup and the stored form
/// would still be the credential.
#[must_use]
pub fn hash_mfa_code(auth_secret: &str, code: &str) -> String {
    let key = hmac::Key::new(hmac::HMAC_SHA256, auth_secret.as_bytes());
    let tag = hmac::sign(&key, format!("{MFA_CONTEXT}\n{code}").as_bytes());

    hex_encode(tag.as_ref())
}

/// Whether a presented MFA code matches a stored keyed digest, in constant
/// time.
#[must_use]
pub fn mfa_code_matches(auth_secret: &str, presented: &str, stored_hash: &str) -> bool {
    let presented_hash = hash_mfa_code(auth_secret, presented);

    bool::from(presented_hash.as_bytes().ct_eq(stored_hash.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_stable_hex_and_hides_the_value() {
        let hash = hash_security_value("reset-token-abc");

        assert_eq!(hash.len(), 64, "SHA-256 as hex");
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(!hash.contains("reset-token-abc"));
        assert_eq!(hash, hash_security_value("reset-token-abc"), "stable");
        assert_ne!(hash, hash_security_value("reset-token-abd"));
    }

    #[test]
    fn matches_only_the_original_value() {
        let stored = hash_security_value("SbGqvFoaOgLpTsAiNwUeRxYcZkMdJhBv");

        assert!(security_value_matches(
            "SbGqvFoaOgLpTsAiNwUeRxYcZkMdJhBv",
            &stored
        ));
        assert!(!security_value_matches(
            "SbGqvFoaOgLpTsAiNwUeRxYcZkMdJhBw",
            &stored
        ));
        assert!(!security_value_matches("", &stored));
    }

    /// The whole point of keying: the stored form of a six-digit code must
    /// not be reproducible from the code alone, or a table over all 10^6
    /// inverts it.
    #[test]
    fn an_mfa_digest_cannot_be_recomputed_without_the_secret() {
        let stored = hash_mfa_code("server-secret", "123456");

        assert_ne!(stored, hash_security_value("123456"), "not a bare digest");
        assert_ne!(
            stored,
            hash_mfa_code("another-secret", "123456"),
            "the secret is part of the digest"
        );
        assert_eq!(stored.len(), 64);
    }

    #[test]
    fn an_mfa_code_matches_only_under_its_own_secret() {
        let stored = hash_mfa_code("server-secret", "123456");

        assert!(mfa_code_matches("server-secret", "123456", &stored));
        assert!(!mfa_code_matches("server-secret", "123457", &stored));
        assert!(
            !mfa_code_matches("another-secret", "123456", &stored),
            "a rotated secret invalidates outstanding codes"
        );
    }
}
