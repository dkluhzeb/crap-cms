//! `crap.crypto` namespace — sha256, hmac, base64, AES-GCM encrypt/decrypt, `random_bytes`.

use std::fmt::Write as _;

use aes_gcm::{Aes256Gcm, KeyInit, Nonce, aead::Aead};
use anyhow::Result;
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use mlua::{Error::RuntimeError, Lua, Result as LuaResult};
use rand::RngCore;
use ring::{digest, hmac};
use subtle::ConstantTimeEq;

use crate::typegen::lua::{LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table};

/// Auth-secret-derived state for the `crap.crypto.encrypt` /
/// `crap.crypto.decrypt` helpers. The 6 other `crap.crypto.*` fns are
/// stateless and use the `()` table below.
pub(super) struct CryptoState {
    secret: String,
}

// ── Stateless crypto helpers ─────────────────────────────────────────

/// SHA-256 hash of a string, returned as hex.
#[lua_fn(path = "crap.crypto.sha256", returns_doc = "64-character hex string.")]
fn crypto_sha256(_: &Lua, #[lua(doc = "Data to hash.")] data: String) -> LuaResult<String> {
    Ok(sha256(&data))
}

/// HMAC-SHA256 of data with a key, returned as hex.
/// Always compare HMACs with `crap.crypto.constant_time_eq(...)`, never with
/// `==`, to avoid timing attacks.
#[lua_fn(
    path = "crap.crypto.hmac_sha256",
    returns_doc = "64-character hex string."
)]
fn crypto_hmac_sha256(
    _: &Lua,
    #[lua(doc = "Data to authenticate.")] data: String,
    #[lua(doc = "HMAC key.")] key: String,
) -> LuaResult<String> {
    Ok(hmac_sha256(&data, &key))
}

/// Constant-time byte-string equality. Use this — never `==` — to compare
/// HMAC tags, tokens, or any secret value. Length mismatch and content
/// mismatch are indistinguishable from the return value.
#[lua_fn(
    path = "crap.crypto.constant_time_eq",
    returns_doc = "True iff `a` equals `b`."
)]
fn crypto_constant_time_eq(_: &Lua, a: String, b: String) -> LuaResult<bool> {
    Ok(constant_time_eq(&a, &b))
}

/// Base64-encode a string.
#[lua_fn(path = "crap.crypto.base64_encode")]
fn crypto_base64_encode(_: &Lua, str: String) -> LuaResult<String> {
    Ok(b64_encode(&str))
}

/// Base64-decode a string.
#[lua_fn(path = "crap.crypto.base64_decode")]
fn crypto_base64_decode(_: &Lua, str: String) -> LuaResult<String> {
    b64_decode(&str)
}

/// Generate `n` random bytes, returned as hex.
#[lua_fn(
    path = "crap.crypto.random_bytes",
    returns_doc = "Hex string of length 2*n."
)]
fn crypto_random_bytes(
    _: &Lua,
    #[lua(doc = "Number of random bytes.")] n: usize,
) -> LuaResult<String> {
    random_bytes(n)
}

// ── Stateful crypto helpers (use the auth secret) ────────────────────

/// Encrypt plaintext using AES-256-GCM. Key derived from auth secret.
/// Returns base64-encoded ciphertext (nonce prepended).
#[lua_fn(
    path = "crap.crypto.encrypt",
    returns_doc = "Base64-encoded ciphertext."
)]
fn crypto_encrypt(state: &CryptoState, _: &Lua, plaintext: String) -> LuaResult<String> {
    encrypt(&state.secret, &plaintext)
}

/// Decrypt ciphertext produced by `encrypt`.
#[lua_fn(
    path = "crap.crypto.decrypt",
    returns_doc = "Decoded plaintext string."
)]
fn crypto_decrypt(
    state: &CryptoState,
    _: &Lua,
    #[lua(doc = "Base64-encoded ciphertext from `encrypt`.")] ciphertext: String,
) -> LuaResult<String> {
    decrypt(&state.secret, &ciphertext)
}

lua_table! {
    name: crap_crypto_stateless,
    path: "crap.crypto",
    state: (),
    header: "Cryptographic helpers. Keys derived from the auth secret in crap.toml.",
    fns: [
        crypto_sha256,
        crypto_hmac_sha256,
        crypto_constant_time_eq,
        crypto_base64_encode,
        crypto_base64_decode,
        crypto_random_bytes,
    ],
}

lua_table! {
    name: crap_crypto_stateful,
    path: "crap.crypto",
    state: CryptoState,
    fns: [crypto_encrypt, crypto_decrypt],
}

/// Register `crap.crypto` — sha256, hmac, base64, AES-GCM encrypt/decrypt, `random_bytes`.
/// Parent `crap` table must already be in globals.
pub(super) fn register_crypto(lua: &Lua, auth_secret: &str) -> Result<()> {
    register_crap_crypto_stateless(lua, ())?;
    register_crap_crypto_stateful(
        lua,
        CryptoState {
            secret: auth_secret.to_string(),
        },
    )?;
    Ok(())
}

/// SHA-256 hash of a string, returned as hex.
fn sha256(data: &str) -> String {
    hex_encode(digest::digest(&digest::SHA256, data.as_bytes()).as_ref())
}

/// HMAC-SHA256 of data with key, returned as hex.
fn hmac_sha256(data: &str, key: &str) -> String {
    let k = hmac::Key::new(hmac::HMAC_SHA256, key.as_bytes());

    hex_encode(hmac::sign(&k, data.as_bytes()).as_ref())
}

/// Constant-time byte-string equality.
///
/// Use this — never `==` — to compare HMAC tags, tokens, or any secret
/// value: Lua's `==` on strings short-circuits on the first differing byte
/// and leaks the match length through timing. The `subtle` crate's
/// `ConstantTimeEq` runs in time independent of where (or whether) the
/// bytes differ.
///
/// Length-mismatch is treated the same as content-mismatch (both return
/// `false`) so the caller cannot distinguish them via return value.
fn constant_time_eq(a: &str, b: &str) -> bool {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();

    if a_bytes.len() != b_bytes.len() {
        return false;
    }

    a_bytes.ct_eq(b_bytes).into()
}

/// Base64-encode a string.
fn b64_encode(data: &str) -> String {
    B64.encode(data.as_bytes())
}

/// Base64-decode a string.
fn b64_decode(data: &str) -> LuaResult<String> {
    let bytes = B64
        .decode(data.as_bytes())
        .map_err(|e| RuntimeError(format!("base64 decode error: {e:#}")))?;
    String::from_utf8(bytes).map_err(|e| RuntimeError(format!("base64 decode utf8 error: {e:#}")))
}

/// Generate `n` random bytes, returned as hex.
fn random_bytes(n: usize) -> LuaResult<String> {
    const MAX: usize = 1024 * 1024;

    if n > MAX {
        return Err(RuntimeError(format!(
            "random_bytes: requested {n} bytes exceeds maximum of {MAX}"
        )));
    }

    let mut buf = vec![0u8; n];

    rand::rng().fill_bytes(&mut buf);

    Ok(hex_encode(&buf))
}

/// AES-256-GCM encrypt: returns base64(nonce || ciphertext).
fn encrypt(secret: &str, plaintext: &str) -> LuaResult<String> {
    let key_hash = digest::digest(&digest::SHA256, secret.as_bytes());
    let cipher = Aes256Gcm::new_from_slice(key_hash.as_ref())
        .map_err(|e| RuntimeError(format!("cipher init: {e:#}")))?;

    let mut nonce_bytes = [0u8; 12];

    rand::rng().fill_bytes(&mut nonce_bytes);

    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_bytes())
        .map_err(|e| RuntimeError(format!("encrypt error: {e:#}")))?;

    let mut combined = nonce_bytes.to_vec();

    combined.extend_from_slice(&ciphertext);

    Ok(B64.encode(&combined))
}

/// AES-256-GCM decrypt: expects base64(nonce || ciphertext).
fn decrypt(secret: &str, encoded: &str) -> LuaResult<String> {
    let combined = B64
        .decode(encoded.as_bytes())
        .map_err(|e| RuntimeError(format!("base64 decode: {e:#}")))?;

    if combined.len() < 12 {
        return Err(RuntimeError("ciphertext too short".into()));
    }

    let (nonce_bytes, ciphertext) = combined.split_at(12);
    let key_hash = digest::digest(&digest::SHA256, secret.as_bytes());
    let cipher = Aes256Gcm::new_from_slice(key_hash.as_ref())
        .map_err(|e| RuntimeError(format!("cipher init: {e:#}")))?;

    let plaintext = cipher
        .decrypt(Nonce::from_slice(nonce_bytes), ciphertext)
        .map_err(|e| RuntimeError(format!("decrypt error: {e:#}")))?;

    String::from_utf8(plaintext).map_err(|e| RuntimeError(format!("decrypt utf8: {e:#}")))
}

/// Encode bytes as lowercase hex string.
pub(super) fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

#[cfg(test)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::case_sensitive_file_extension_comparisons,
    clippy::items_after_statements,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_panics_doc,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::unreadable_literal,
    clippy::used_underscore_binding
)]
mod tests {
    use super::*;

    fn setup_lua(secret: &str) -> Lua {
        let lua = Lua::new();
        lua.globals()
            .set("crap", lua.create_table().unwrap())
            .unwrap();
        register_crypto(&lua, secret).unwrap();
        lua
    }

    // --- hex_encode ---

    #[test]
    fn hex_encode_empty() {
        assert_eq!(hex_encode(&[]), "");
    }

    #[test]
    fn hex_encode_known_input() {
        assert_eq!(hex_encode(&[0x00, 0xff, 0x0a, 0xab]), "00ff0aab");
    }

    #[test]
    fn hex_encode_single_byte() {
        assert_eq!(hex_encode(&[0x42]), "42");
        assert_eq!(hex_encode(&[0x00]), "00");
        assert_eq!(hex_encode(&[0xff]), "ff");
        assert_eq!(hex_encode(&[0x0a]), "0a");
    }

    #[test]
    fn hex_encode_multiple_bytes() {
        assert_eq!(hex_encode(&[0xde, 0xad, 0xbe, 0xef]), "deadbeef");
        assert_eq!(hex_encode(&[0x01, 0x23, 0x45, 0x67]), "01234567");
    }

    // --- AES-GCM roundtrip ---

    #[test]
    fn aes_gcm_roundtrip() {
        let lua = setup_lua("test-secret-key");
        let result: String = lua
            .load(
                r#"
                local ct = crap.crypto.encrypt("hello, world")

                return crap.crypto.decrypt(ct)
            "#,
            )
            .eval()
            .unwrap();
        assert_eq!(result, "hello, world");
    }

    #[test]
    fn aes_gcm_roundtrip_empty_plaintext() {
        let lua = setup_lua("test-secret-key");
        let result: String = lua
            .load(
                r#"
                local ct = crap.crypto.encrypt("")

                return crap.crypto.decrypt(ct)
            "#,
            )
            .eval()
            .unwrap();
        assert_eq!(result, "");
    }

    #[test]
    fn aes_gcm_two_encryptions_differ() {
        // Same plaintext encrypted twice should produce different ciphertext (random nonce).
        let lua = setup_lua("test-secret-key");
        let result: bool = lua
            .load(
                r#"
                local ct1 = crap.crypto.encrypt("same plaintext")
                local ct2 = crap.crypto.encrypt("same plaintext")

                return ct1 ~= ct2
            "#,
            )
            .eval()
            .unwrap();
        assert!(result);
    }

    #[test]
    fn aes_gcm_decrypt_wrong_key_fails() {
        let lua_enc = setup_lua("key-one");
        let lua_dec = setup_lua("key-two");

        let ciphertext: String = lua_enc
            .load(r#"return crap.crypto.encrypt("secret")"#)
            .eval()
            .unwrap();

        // Decrypting with a different key should fail.
        let result = lua_dec
            .load(format!(r#"return crap.crypto.decrypt("{ciphertext}")"#))
            .eval::<String>();
        assert!(result.is_err());
    }

    #[test]
    fn aes_gcm_decrypt_short_ciphertext_fails() {
        let lua = setup_lua("test-secret-key");
        // base64("tooshort") has fewer than 12 decoded bytes.
        let result = lua
            .load(r#"return crap.crypto.decrypt("dG9vc2hvcnQ=")"#)
            .eval::<String>();
        assert!(result.is_err());
    }

    #[test]
    fn aes_gcm_decrypt_invalid_base64_fails() {
        let lua = setup_lua("test-secret-key");
        let result = lua
            .load(r#"return crap.crypto.decrypt("not valid base64!!!")"#)
            .eval::<String>();
        assert!(result.is_err());
    }

    // --- base64 ---

    #[test]
    fn base64_roundtrip() {
        let lua = setup_lua("s");
        let result: String = lua
            .load(
                r#"
                local encoded = crap.crypto.base64_encode("hello base64")

                return crap.crypto.base64_decode(encoded)
            "#,
            )
            .eval()
            .unwrap();
        assert_eq!(result, "hello base64");
    }

    #[test]
    fn base64_encode_known_value() {
        let lua = setup_lua("s");
        let result: String = lua
            .load(r#"return crap.crypto.base64_encode("Man")"#)
            .eval()
            .unwrap();
        // "Man" encodes to "TWFu" in standard base64.
        assert_eq!(result, "TWFu");
    }

    #[test]
    fn base64_decode_invalid_input_fails() {
        let lua = setup_lua("s");
        let result = lua
            .load(r#"return crap.crypto.base64_decode("not!valid base64@@")"#)
            .eval::<String>();
        assert!(result.is_err());
    }

    // --- SHA-256 ---

    #[test]
    fn sha256_known_hash() {
        let lua = setup_lua("s");
        let result: String = lua
            .load(r#"return crap.crypto.sha256("hello")"#)
            .eval()
            .unwrap();
        // SHA-256("hello") = 2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824
        assert_eq!(
            result,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn sha256_empty_string() {
        let lua = setup_lua("s");
        let result: String = lua.load(r#"return crap.crypto.sha256("")"#).eval().unwrap();
        // SHA-256("") = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
        assert_eq!(
            result,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    // --- HMAC-SHA256 ---

    #[test]
    fn hmac_sha256_known_output() {
        let lua = setup_lua("s");
        let result: String = lua
            .load(r#"return crap.crypto.hmac_sha256("message", "key")"#)
            .eval()
            .unwrap();
        // HMAC-SHA256("message", "key") — verified via standard test vectors.
        assert_eq!(
            result,
            "6e9ef29b75fffc5b7abae527d58fdadb2fe42e7219011976917343065f58ed4a"
        );
    }

    // --- constant_time_eq ---

    #[test]
    fn constant_time_eq_true_on_equal() {
        let lua = setup_lua("s");
        let result: bool = lua
            .load(r#"return crap.crypto.constant_time_eq("abcdef", "abcdef")"#)
            .eval()
            .unwrap();
        assert!(result);
    }

    #[test]
    fn constant_time_eq_false_on_different_length() {
        let lua = setup_lua("s");
        let result: bool = lua
            .load(r#"return crap.crypto.constant_time_eq("abc", "abcdef")"#)
            .eval()
            .unwrap();
        assert!(!result);
    }

    #[test]
    fn constant_time_eq_false_on_different_content() {
        let lua = setup_lua("s");
        let result: bool = lua
            .load(r#"return crap.crypto.constant_time_eq("abcdef", "abcxef")"#)
            .eval()
            .unwrap();
        assert!(!result);
    }

    #[test]
    fn constant_time_eq_true_on_empty() {
        let lua = setup_lua("s");
        let result: bool = lua
            .load(r#"return crap.crypto.constant_time_eq("", "")"#)
            .eval()
            .unwrap();
        assert!(result);
    }

    #[test]
    fn hmac_sha256_different_keys_differ() {
        let lua = setup_lua("s");
        let result: bool = lua
            .load(
                r#"
                local h1 = crap.crypto.hmac_sha256("data", "key1")
                local h2 = crap.crypto.hmac_sha256("data", "key2")

                return h1 ~= h2
            "#,
            )
            .eval()
            .unwrap();
        assert!(result);
    }

    // --- random_bytes ---

    #[test]
    fn random_bytes_correct_length() {
        let lua = setup_lua("s");
        // Each byte encodes to 2 hex chars.
        let result: String = lua
            .load(r"return crap.crypto.random_bytes(16)")
            .eval()
            .unwrap();
        assert_eq!(result.len(), 32);
    }

    #[test]
    fn random_bytes_zero_length() {
        let lua = setup_lua("s");
        let result: String = lua
            .load(r"return crap.crypto.random_bytes(0)")
            .eval()
            .unwrap();
        assert_eq!(result, "");
    }

    #[test]
    fn random_bytes_over_limit_errors() {
        let lua = setup_lua("s");
        let result = lua
            .load("return crap.crypto.random_bytes(1048577)") // 1 MB + 1
            .eval::<String>();
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("exceeds maximum"), "unexpected error: {err}");
    }

    #[test]
    fn random_bytes_at_limit_succeeds() {
        let lua = setup_lua("s");
        let result: String = lua
            .load("return crap.crypto.random_bytes(1048576)") // exactly 1 MB
            .eval()
            .unwrap();
        assert_eq!(result.len(), 1048576 * 2); // hex encoding doubles length
    }

    #[test]
    fn random_bytes_two_calls_differ() {
        let lua = setup_lua("s");
        let result: bool = lua
            .load(
                r"
                local a = crap.crypto.random_bytes(32)
                local b = crap.crypto.random_bytes(32)

                return a ~= b
            ",
            )
            .eval()
            .unwrap();
        assert!(result);
    }
}
