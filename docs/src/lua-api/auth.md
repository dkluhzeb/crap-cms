# crap.auth

Password hashing and verification helpers using Argon2id.

## Functions

### `crap.auth.hash_password(password)`

Hash a plaintext password using Argon2id.

**Parameters:**
- `password` (string) — Plaintext password.

**Returns:** string — The hashed password string.

```lua
local hash = crap.auth.hash_password("secret123")
-- hash is an Argon2id hash string like "$argon2id$v=19$..."
```

### `crap.auth.verify_password(password, hash)`

Verify a plaintext password against a stored hash.

**Parameters:**
- `password` (string) — Plaintext password to check.
- `hash` (string) — Stored Argon2id hash.

**Returns:** boolean — `true` if the password matches.

```lua
local valid = crap.auth.verify_password("secret123", stored_hash)
if valid then
    crap.log.info("Password matches")
end
```

## Notes

- Available in both init.lua and hooks.
- Uses the same Argon2id implementation as the built-in auth system.
- Useful for custom auth strategies or migrating users from external systems.
- For custom authentication logic, use [auth strategies](../authentication/custom-strategies.md) or [auth callbacks](../authentication/custom-strategies.md#auth-callbacks).
