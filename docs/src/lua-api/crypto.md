# crap.crypto

Cryptographic helpers. AES-256-GCM encryption key is derived from the `auth.secret` in `crap.toml`.

## crap.crypto.sha256(data)

SHA-256 hash of a string, returned as a 64-character hex string.

```lua
local hash = crap.crypto.sha256("hello world")
-- "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
```

## crap.crypto.hmac_sha256(data, key)

HMAC-SHA256 of data with a key, returned as hex.

```lua
local mac = crap.crypto.hmac_sha256("message", "secret-key")
```

## crap.crypto.base64_encode(str) / crap.crypto.base64_decode(str)

Base64 encoding and decoding.

```lua
local encoded = crap.crypto.base64_encode("hello")  -- "aGVsbG8="
local decoded = crap.crypto.base64_decode(encoded)   -- "hello"
```

## crap.crypto.encrypt(plaintext) / crap.crypto.decrypt(ciphertext)

AES-256-GCM encryption using the auth secret from `crap.toml`. The encrypted output is base64-encoded with a random nonce prepended.

```lua
local encrypted = crap.crypto.encrypt("sensitive data")
local original = crap.crypto.decrypt(encrypted)  -- "sensitive data"
```

> **Note:** The encryption key is derived from `auth.secret` in `crap.toml` — the same secret used for JWT signing. Rotating the JWT secret will invalidate all previously encrypted data. If you rotate secrets, you must re-encrypt any data that was encrypted with the old secret.

## crap.crypto.random_bytes(n)

Generate `n` random bytes, returned as a hex string of length `2*n`.

```lua
local token = crap.crypto.random_bytes(16)  -- 32-character hex string
```
