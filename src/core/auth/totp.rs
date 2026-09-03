//! TOTP (RFC 6238) second factor: secret generation, provisioning URIs,
//! code verification, and sealed at-rest storage.
//!
//! `mfa = "totp"` verifies a 6-digit code from an authenticator app against
//! a per-user shared secret — no code delivery at all (unlike `email` /
//! `custom`). The secret is generated server-side on the first MFA
//! challenge, stored **sealed** (AES-256-GCM keyed from `[auth] secret`
//! with a TOTP-specific domain context), and confirmed by the first
//! successful verification; the provisioning URI is only ever shown while
//! unconfirmed.
//!
//! Standard parameters: SHA-1 (the authenticator-app ecosystem default),
//! 30-second steps, 6 digits, ±1 step verification window. Replay is
//! prevented by persisting the last accepted step — a code never verifies
//! twice, and older steps than the last accepted one are rejected.

use aes_gcm::{Aes256Gcm, KeyInit, Nonce, aead::Aead};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use rand::RngCore;
use ring::{digest, hmac};
use subtle::ConstantTimeEq;

/// TOTP time-step in seconds (RFC 6238 default).
pub const TOTP_STEP_SECS: i64 = 30;

/// Verification window: current step ±1 (clock skew tolerance).
const WINDOW: i64 = 1;

/// Domain context for sealing the stored secret — distinct from the
/// `crap.crypto.encrypt` key derivation so the two ciphertext families are
/// not interchangeable.
const SEAL_CONTEXT: &str = "crap-cms:totp-secret:v1";

/// RFC 4648 base32 alphabet (no padding, uppercase) — what authenticator
/// apps accept as the manual-entry key.
const BASE32_ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

fn base32_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(5) * 8);
    let mut buffer: u64 = 0;
    let mut bits: u32 = 0;

    for &byte in data {
        buffer = (buffer << 8) | u64::from(byte);
        bits += 8;

        while bits >= 5 {
            bits -= 5;
            let idx = ((buffer >> bits) & 0x1f) as usize;
            out.push(BASE32_ALPHABET[idx] as char);
        }
    }

    if bits > 0 {
        let idx = ((buffer << (5 - bits)) & 0x1f) as usize;
        out.push(BASE32_ALPHABET[idx] as char);
    }

    out
}

fn base32_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 5 / 8);
    let mut buffer: u64 = 0;
    let mut bits: u32 = 0;

    for c in s.bytes() {
        let value = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a',
            b'2'..=b'7' => c - b'2' + 26,
            b'=' => continue,
            _ => return None,
        };

        buffer = (buffer << 5) | u64::from(value);
        bits += 5;

        if bits >= 8 {
            bits -= 8;
            out.push(((buffer >> bits) & 0xff) as u8);
        }
    }

    Some(out)
}

/// Generate a fresh 160-bit TOTP secret, base32-encoded (the manual key).
#[must_use]
pub fn generate_totp_secret() -> String {
    let mut bytes = [0u8; 20];
    rand::rng().fill_bytes(&mut bytes);

    base32_encode(&bytes)
}

/// The `otpauth://` provisioning URI authenticator apps consume (as a QR
/// payload or a tap-to-add link). `account` (the user's email) is
/// percent-encoded; the issuer identifies this CMS.
#[must_use]
pub fn provisioning_uri(account: &str, secret_b32: &str) -> String {
    let account: String = account
        .bytes()
        .flat_map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'~') {
                vec![b as char]
            } else {
                format!("%{b:02X}").chars().collect()
            }
        })
        .collect();

    format!(
        "otpauth://totp/crap-cms:{account}?secret={secret_b32}&issuer=crap-cms&digits=6&period=30"
    )
}

/// Compute the 6-digit code for one time step.
fn totp_code(secret: &[u8], step: i64) -> String {
    let key = hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, secret);
    let tag = hmac::sign(&key, &step.to_be_bytes());
    let tag = tag.as_ref();

    // RFC 4226 dynamic truncation.
    let offset = (tag[tag.len() - 1] & 0x0f) as usize;
    let binary = (u32::from(tag[offset] & 0x7f) << 24)
        | (u32::from(tag[offset + 1]) << 16)
        | (u32::from(tag[offset + 2]) << 8)
        | u32::from(tag[offset + 3]);

    format!("{:06}", binary % 1_000_000)
}

/// Verify a TOTP code against the base32 secret at `now_unix`, tolerating
/// ±1 step of clock skew. `last_step` is the most recently accepted step —
/// any step at or before it is rejected (single-use codes, no replay).
///
/// Returns the accepted step on success (persist it as the new
/// `last_step`), `None` on any failure.
#[must_use]
pub fn verify_totp(
    secret_b32: &str,
    code: &str,
    now_unix: i64,
    last_step: Option<i64>,
) -> Option<i64> {
    let secret = base32_decode(secret_b32)?;
    if secret.is_empty() || code.len() != 6 {
        return None;
    }

    let current = now_unix.div_euclid(TOTP_STEP_SECS);

    for step in (current - WINDOW)..=(current + WINDOW) {
        if step <= last_step.unwrap_or(i64::MIN) {
            continue;
        }

        let expected = totp_code(&secret, step);
        if bool::from(expected.as_bytes().ct_eq(code.as_bytes())) {
            return Some(step);
        }
    }

    None
}

/// The valid code for `secret_b32` at `now_unix` — for tests and tooling
/// (the verification path never uses it directly).
#[must_use]
pub fn totp_code_at(secret_b32: &str, now_unix: i64) -> Option<String> {
    let secret = base32_decode(secret_b32)?;
    if secret.is_empty() {
        return None;
    }

    Some(totp_code(&secret, now_unix.div_euclid(TOTP_STEP_SECS)))
}

fn seal_key(auth_secret: &str) -> [u8; 32] {
    let material = format!("{SEAL_CONTEXT}\n{auth_secret}");
    let hash = digest::digest(&digest::SHA256, material.as_bytes());

    let mut key = [0u8; 32];
    key.copy_from_slice(hash.as_ref());
    key
}

/// Seal the base32 secret for at-rest storage: AES-256-GCM,
/// base64(nonce || ciphertext), keyed from `[auth] secret` with a
/// TOTP-specific domain context. `None` when the auth secret is empty.
#[must_use]
pub fn seal_totp_secret(auth_secret: &str, secret_b32: &str) -> Option<String> {
    if auth_secret.is_empty() {
        return None;
    }

    let cipher = Aes256Gcm::new_from_slice(&seal_key(auth_secret)).ok()?;

    let mut nonce_bytes = [0u8; 12];
    rand::rng().fill_bytes(&mut nonce_bytes);

    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), secret_b32.as_bytes())
        .ok()?;

    let mut combined = nonce_bytes.to_vec();
    combined.extend_from_slice(&ciphertext);

    Some(B64.encode(&combined))
}

/// Open a sealed secret. `None` on any failure (wrong key, tamper, junk).
#[must_use]
pub fn open_totp_secret(auth_secret: &str, sealed: &str) -> Option<String> {
    if auth_secret.is_empty() {
        return None;
    }

    let combined = B64.decode(sealed.as_bytes()).ok()?;
    if combined.len() < 12 {
        return None;
    }

    let (nonce, ciphertext) = combined.split_at(12);
    let cipher = Aes256Gcm::new_from_slice(&seal_key(auth_secret)).ok()?;
    let plaintext = cipher.decrypt(Nonce::from_slice(nonce), ciphertext).ok()?;

    String::from_utf8(plaintext).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 6238 Appendix B test vectors (SHA-1), truncated to 6 digits
    /// (the 6-digit code is the 8-digit vector mod 10^6).
    #[test]
    fn rfc6238_sha1_vectors() {
        let secret = base32_encode(b"12345678901234567890");

        for (time, code) in [
            (59_i64, "287082"),
            (1_111_111_109, "081804"),
            (1_111_111_111, "050471"),
            (1_234_567_890, "005924"),
            (2_000_000_000, "279037"),
            (20_000_000_000, "353130"),
        ] {
            assert_eq!(
                verify_totp(&secret, code, time, None),
                Some(time.div_euclid(30)),
                "vector at t={time}"
            );
        }
    }

    #[test]
    fn window_tolerates_one_step_of_skew() {
        let secret = base32_encode(b"12345678901234567890");

        // Code for t=59 (step 1) still verifies at t=89 (step 2)…
        assert_eq!(verify_totp(&secret, "287082", 89, None), Some(1));
        // …but not at t=119 (step 3, outside the window).
        assert_eq!(verify_totp(&secret, "287082", 119, None), None);
    }

    #[test]
    fn replay_is_rejected() {
        let secret = base32_encode(b"12345678901234567890");

        let step = verify_totp(&secret, "287082", 59, None).expect("first use");
        assert_eq!(
            verify_totp(&secret, "287082", 59, Some(step)),
            None,
            "the same code must never verify twice"
        );
    }

    #[test]
    fn wrong_code_fails() {
        let secret = base32_encode(b"12345678901234567890");
        assert_eq!(verify_totp(&secret, "000000", 59, None), None);
        assert_eq!(verify_totp(&secret, "28708", 59, None), None, "length");
        assert_eq!(verify_totp(&secret, "", 59, None), None);
    }

    #[test]
    fn base32_round_trips() {
        for data in [&b"12345678901234567890"[..], b"", b"a", b"hello world!"] {
            let encoded = base32_encode(data);
            assert_eq!(base32_decode(&encoded).as_deref(), Some(data));
        }
        // Lowercase and padding are tolerated on decode.
        assert_eq!(base32_decode("mzxw6===").as_deref(), Some(&b"foo"[..]));
        assert!(base32_decode("not base32!").is_none());
    }

    #[test]
    fn generated_secret_is_base32_and_verifiable() {
        let secret = generate_totp_secret();
        assert_eq!(secret.len(), 32, "160 bits → 32 base32 chars");
        assert!(base32_decode(&secret).is_some());
    }

    #[test]
    fn provisioning_uri_encodes_account() {
        let uri = provisioning_uri("user+x@example.com", "ABC234");
        assert!(uri.starts_with("otpauth://totp/crap-cms:user%2Bx%40example.com?"));
        assert!(uri.contains("secret=ABC234"));
        assert!(uri.contains("issuer=crap-cms"));
    }

    #[test]
    fn seal_open_round_trip() {
        let sealed = seal_totp_secret("app-secret", "MYSECRET234").expect("seal");
        assert_eq!(
            open_totp_secret("app-secret", &sealed).as_deref(),
            Some("MYSECRET234")
        );

        assert!(
            open_totp_secret("other-secret", &sealed).is_none(),
            "wrong key"
        );
        assert!(open_totp_secret("app-secret", "junk").is_none());
        assert!(
            seal_totp_secret("", "X").is_none(),
            "empty secret never seals"
        );
    }

    /// The sealing keys for TOTP and `crap.crypto.encrypt` must differ — the
    /// domain context guarantees the two ciphertext families are not
    /// interchangeable even though both derive from `[auth] secret`.
    #[test]
    fn seal_key_is_domain_separated() {
        let plain_sha = digest::digest(&digest::SHA256, b"app-secret");
        assert_ne!(seal_key("app-secret").as_slice(), plain_sha.as_ref());
    }
}
