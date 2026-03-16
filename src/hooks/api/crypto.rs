//! `crap.crypto` namespace — sha256, hmac, base64, AES-GCM encrypt/decrypt, random_bytes.

use anyhow::Result;
use mlua::{Lua, Table};

/// Register `crap.crypto` — sha256, hmac, base64, AES-GCM encrypt/decrypt, random_bytes.
pub(super) fn register_crypto(lua: &Lua, crap: &Table, auth_secret: &str) -> Result<()> {
    let crypto_table = lua.create_table()?;

    let sha256_fn = lua.create_function(|_, data: String| -> mlua::Result<String> {
        use ring::digest;
        let hash = digest::digest(&digest::SHA256, data.as_bytes());
        Ok(hex_encode(hash.as_ref()))
    })?;
    crypto_table.set("sha256", sha256_fn)?;

    let hmac_sha256_fn =
        lua.create_function(|_, (data, key): (String, String)| -> mlua::Result<String> {
            use ring::hmac;
            let k = hmac::Key::new(hmac::HMAC_SHA256, key.as_bytes());
            let tag = hmac::sign(&k, data.as_bytes());
            Ok(hex_encode(tag.as_ref()))
        })?;
    crypto_table.set("hmac_sha256", hmac_sha256_fn)?;

    let b64_encode_fn = lua.create_function(|_, data: String| -> mlua::Result<String> {
        use base64::Engine;
        Ok(base64::engine::general_purpose::STANDARD.encode(data.as_bytes()))
    })?;
    crypto_table.set("base64_encode", b64_encode_fn)?;

    let b64_decode_fn = lua.create_function(|_, data: String| -> mlua::Result<String> {
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data.as_bytes())
            .map_err(|e| mlua::Error::RuntimeError(format!("base64 decode error: {}", e)))?;
        String::from_utf8(bytes)
            .map_err(|e| mlua::Error::RuntimeError(format!("base64 decode utf8 error: {}", e)))
    })?;
    crypto_table.set("base64_decode", b64_decode_fn)?;

    let secret = auth_secret.to_string();
    let encrypt_fn = lua.create_function(move |_, plaintext: String| -> mlua::Result<String> {
        use aes_gcm::Nonce;
        use aes_gcm::{Aes256Gcm, KeyInit, aead::Aead};
        use base64::Engine;
        use rand::RngCore;
        use ring::digest;

        let key_hash = digest::digest(&digest::SHA256, secret.as_bytes());
        let cipher = Aes256Gcm::new_from_slice(key_hash.as_ref())
            .map_err(|e| mlua::Error::RuntimeError(format!("cipher init: {}", e)))?;

        let mut nonce_bytes = [0u8; 12];
        rand::rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = cipher
            .encrypt(nonce, plaintext.as_bytes())
            .map_err(|e| mlua::Error::RuntimeError(format!("encrypt error: {}", e)))?;

        let mut combined = nonce_bytes.to_vec();
        combined.extend_from_slice(&ciphertext);
        Ok(base64::engine::general_purpose::STANDARD.encode(&combined))
    })?;
    crypto_table.set("encrypt", encrypt_fn)?;

    let secret2 = auth_secret.to_string();
    let decrypt_fn = lua.create_function(move |_, encoded: String| -> mlua::Result<String> {
        use aes_gcm::Nonce;
        use aes_gcm::{Aes256Gcm, KeyInit, aead::Aead};
        use base64::Engine;
        use ring::digest;

        let combined = base64::engine::general_purpose::STANDARD
            .decode(encoded.as_bytes())
            .map_err(|e| mlua::Error::RuntimeError(format!("base64 decode: {}", e)))?;

        if combined.len() < 12 {
            return Err(mlua::Error::RuntimeError("ciphertext too short".into()));
        }
        let (nonce_bytes, ciphertext) = combined.split_at(12);
        let nonce = Nonce::from_slice(nonce_bytes);

        let key_hash = digest::digest(&digest::SHA256, secret2.as_bytes());
        let cipher = Aes256Gcm::new_from_slice(key_hash.as_ref())
            .map_err(|e| mlua::Error::RuntimeError(format!("cipher init: {}", e)))?;

        let plaintext = cipher
            .decrypt(nonce, ciphertext)
            .map_err(|e| mlua::Error::RuntimeError(format!("decrypt error: {}", e)))?;

        String::from_utf8(plaintext)
            .map_err(|e| mlua::Error::RuntimeError(format!("decrypt utf8: {}", e)))
    })?;
    crypto_table.set("decrypt", decrypt_fn)?;

    let random_bytes_fn = lua.create_function(|_, n: usize| -> mlua::Result<String> {
        const MAX_RANDOM_BYTES: usize = 1024 * 1024; // 1 MB
        if n > MAX_RANDOM_BYTES {
            return Err(mlua::Error::RuntimeError(format!(
                "random_bytes: requested {} bytes exceeds maximum of {}",
                n, MAX_RANDOM_BYTES
            )));
        }
        use rand::RngCore;
        let mut buf = vec![0u8; n];
        rand::rng().fill_bytes(&mut buf);
        Ok(hex_encode(&buf))
    })?;
    crypto_table.set("random_bytes", random_bytes_fn)?;

    crap.set("crypto", crypto_table)?;
    Ok(())
}

/// Encode bytes as lowercase hex string.
pub(super) fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_lua(secret: &str) -> Lua {
        let lua = Lua::new();
        let crap = lua.create_table().unwrap();
        register_crypto(&lua, &crap, secret).unwrap();
        lua.globals().set("crap", crap).unwrap();
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
            .load(format!(r#"return crap.crypto.decrypt("{}")"#, ciphertext))
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
            .load(r#"return crap.crypto.random_bytes(16)"#)
            .eval()
            .unwrap();
        assert_eq!(result.len(), 32);
    }

    #[test]
    fn random_bytes_zero_length() {
        let lua = setup_lua("s");
        let result: String = lua
            .load(r#"return crap.crypto.random_bytes(0)"#)
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
                r#"
                local a = crap.crypto.random_bytes(32)
                local b = crap.crypto.random_bytes(32)

                return a ~= b
            "#,
            )
            .eval()
            .unwrap();
        assert!(result);
    }
}
