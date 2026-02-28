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

    let hmac_sha256_fn = lua.create_function(|_, (data, key): (String, String)| -> mlua::Result<String> {
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
        let bytes = base64::engine::general_purpose::STANDARD.decode(data.as_bytes())
            .map_err(|e| mlua::Error::RuntimeError(format!("base64 decode error: {}", e)))?;
        String::from_utf8(bytes)
            .map_err(|e| mlua::Error::RuntimeError(format!("base64 decode utf8 error: {}", e)))
    })?;
    crypto_table.set("base64_decode", b64_decode_fn)?;

    let secret = auth_secret.to_string();
    let encrypt_fn = lua.create_function(move |_, plaintext: String| -> mlua::Result<String> {
        use aes_gcm::{Aes256Gcm, KeyInit, aead::Aead};
        use aes_gcm::Nonce;
        use ring::digest;
        use base64::Engine;
        use rand::RngCore;

        let key_hash = digest::digest(&digest::SHA256, secret.as_bytes());
        let cipher = Aes256Gcm::new_from_slice(key_hash.as_ref())
            .map_err(|e| mlua::Error::RuntimeError(format!("cipher init: {}", e)))?;

        let mut nonce_bytes = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = cipher.encrypt(nonce, plaintext.as_bytes())
            .map_err(|e| mlua::Error::RuntimeError(format!("encrypt error: {}", e)))?;

        let mut combined = nonce_bytes.to_vec();
        combined.extend_from_slice(&ciphertext);
        Ok(base64::engine::general_purpose::STANDARD.encode(&combined))
    })?;
    crypto_table.set("encrypt", encrypt_fn)?;

    let secret2 = auth_secret.to_string();
    let decrypt_fn = lua.create_function(move |_, encoded: String| -> mlua::Result<String> {
        use aes_gcm::{Aes256Gcm, KeyInit, aead::Aead};
        use aes_gcm::Nonce;
        use ring::digest;
        use base64::Engine;

        let combined = base64::engine::general_purpose::STANDARD.decode(encoded.as_bytes())
            .map_err(|e| mlua::Error::RuntimeError(format!("base64 decode: {}", e)))?;
        if combined.len() < 12 {
            return Err(mlua::Error::RuntimeError("ciphertext too short".into()));
        }
        let (nonce_bytes, ciphertext) = combined.split_at(12);
        let nonce = Nonce::from_slice(nonce_bytes);

        let key_hash = digest::digest(&digest::SHA256, secret2.as_bytes());
        let cipher = Aes256Gcm::new_from_slice(key_hash.as_ref())
            .map_err(|e| mlua::Error::RuntimeError(format!("cipher init: {}", e)))?;

        let plaintext = cipher.decrypt(nonce, ciphertext)
            .map_err(|e| mlua::Error::RuntimeError(format!("decrypt error: {}", e)))?;

        String::from_utf8(plaintext)
            .map_err(|e| mlua::Error::RuntimeError(format!("decrypt utf8: {}", e)))
    })?;
    crypto_table.set("decrypt", decrypt_fn)?;

    let random_bytes_fn = lua.create_function(|_, n: usize| -> mlua::Result<String> {
        use rand::RngCore;
        let mut buf = vec![0u8; n];
        rand::thread_rng().fill_bytes(&mut buf);
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
