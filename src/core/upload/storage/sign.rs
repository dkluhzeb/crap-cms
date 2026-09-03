//! Signed serve-URLs for uploads — time-boxed HMAC capabilities.
//!
//! The `/uploads/{collection}/{filename}` serve route authenticates via the
//! admin session cookie or a Bearer token. Neither works for a browser
//! loading a *private* file cross-origin (an `<img>` cannot attach a Bearer
//! header, and cross-origin cookies are not sent) — so the backend can mint
//! a **signed URL** instead: the stored proxy path plus `exp` (unix seconds)
//! and `sig` (HMAC-SHA256, hex) query parameters.
//!
//! A valid signature is a *capability*: authorization happened at mint time
//! (server-side, via `crap.uploads.sign_url` from a hook or custom route —
//! typically on data that already passed the read pipeline). The serve route
//! honors it without re-running the per-document access gate. Keep TTLs
//! short; possession of the URL is possession of the file until `exp`.
//!
//! The key is the `[auth] secret`, domain-separated by a versioned context
//! string in the signed message — no extra configuration. Stored `url`
//! values never change (frozen contract): signing is read-time only.

use ring::hmac;

/// Domain-separation context, versioned so a future scheme change can't
/// collide with v1 signatures.
const SIGN_CONTEXT: &str = "crap-cms:upload-url:v1";

/// The exact byte string the HMAC covers.
fn sig_message(path: &str, exp: i64) -> String {
    format!("{SIGN_CONTEXT}\n{path}\n{exp}")
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    // ASCII guard keeps the byte-indexed slicing below total — a multi-byte
    // UTF-8 char at an even offset would otherwise panic on a non-boundary
    // slice. (Unreachable over HTTP today — hyper rejects non-ASCII request
    // targets — but the helper must not rely on its callers for safety.)
    if !s.is_ascii() || !s.len().is_multiple_of(2) {
        return None;
    }

    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// Sign an upload proxy path until `exp` (unix seconds). Returns `None` when
/// `secret` is empty — signing requires a configured `[auth] secret`.
#[must_use]
pub fn sign_upload_path(secret: &str, path: &str, exp: i64) -> Option<String> {
    if secret.is_empty() {
        return None;
    }

    let key = hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes());
    let tag = hmac::sign(&key, sig_message(path, exp).as_bytes());

    Some(hex_encode(tag.as_ref()))
}

/// Build a complete signed URL: `{path}?exp={exp}&sig={sig}`. `None` when the
/// secret is empty or `expires_in` is not positive.
#[must_use]
pub fn signed_upload_url(
    secret: &str,
    path: &str,
    expires_in_secs: i64,
    now_unix: i64,
) -> Option<String> {
    if expires_in_secs <= 0 {
        return None;
    }

    let exp = now_unix.checked_add(expires_in_secs)?;
    let sig = sign_upload_path(secret, path, exp)?;

    Some(format!("{path}?exp={exp}&sig={sig}"))
}

/// Verify a signed-URL pair for `path`. False on: empty secret, expired
/// `exp`, undecodable `sig`, or tag mismatch (`ring`'s verify is
/// constant-time).
#[must_use]
pub fn verify_upload_sig(secret: &str, path: &str, exp: i64, sig: &str, now_unix: i64) -> bool {
    if secret.is_empty() || exp <= now_unix {
        return false;
    }

    let Some(sig_bytes) = hex_decode(sig) else {
        return false;
    };

    let key = hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes());

    hmac::verify(&key, sig_message(path, exp).as_bytes(), &sig_bytes).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "test-secret";
    const PATH: &str = "/uploads/posts/abc_photo.jpg";

    #[test]
    fn sign_verify_round_trip() {
        let url = signed_upload_url(SECRET, PATH, 300, 1_000).expect("signable");
        assert!(url.starts_with("/uploads/posts/abc_photo.jpg?exp=1300&sig="));

        let sig = url.split("&sig=").nth(1).unwrap();
        assert!(verify_upload_sig(SECRET, PATH, 1_300, sig, 1_000));
    }

    #[test]
    fn expired_signature_fails() {
        let sig = sign_upload_path(SECRET, PATH, 1_300).unwrap();
        assert!(!verify_upload_sig(SECRET, PATH, 1_300, &sig, 1_300));
        assert!(!verify_upload_sig(SECRET, PATH, 1_300, &sig, 2_000));
    }

    #[test]
    fn tampered_path_or_exp_fails() {
        let sig = sign_upload_path(SECRET, PATH, 1_300).unwrap();
        assert!(!verify_upload_sig(
            SECRET,
            "/uploads/posts/other.jpg",
            1_300,
            &sig,
            1_000
        ));
        assert!(!verify_upload_sig(SECRET, PATH, 9_999, &sig, 1_000));
    }

    #[test]
    fn wrong_secret_fails() {
        let sig = sign_upload_path(SECRET, PATH, 1_300).unwrap();
        assert!(!verify_upload_sig("other-secret", PATH, 1_300, &sig, 1_000));
    }

    #[test]
    fn empty_secret_never_signs_or_verifies() {
        assert!(sign_upload_path("", PATH, 1_300).is_none());
        assert!(signed_upload_url("", PATH, 300, 1_000).is_none());
        assert!(!verify_upload_sig("", PATH, 1_300, "00", 1_000));
    }

    #[test]
    fn garbage_signature_fails() {
        assert!(!verify_upload_sig(SECRET, PATH, 1_300, "zz-not-hex", 1_000));
        assert!(!verify_upload_sig(SECRET, PATH, 1_300, "abc", 1_000));
    }

    /// Regression: a multi-byte UTF-8 `sig` of even byte length must return
    /// false, not panic on a non-char-boundary slice.
    #[test]
    fn non_ascii_signature_fails_without_panic() {
        assert!(!verify_upload_sig(
            SECRET,
            PATH,
            1_300,
            "\u{1D11E}\u{1D11E}",
            1_000
        ));
    }

    #[test]
    fn non_positive_ttl_refused() {
        assert!(signed_upload_url(SECRET, PATH, 0, 1_000).is_none());
        assert!(signed_upload_url(SECRET, PATH, -5, 1_000).is_none());
    }
}
