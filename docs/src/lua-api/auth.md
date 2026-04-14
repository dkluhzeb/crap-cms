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

### `crap.auth.user()`

Return the currently authenticated user document for the in-flight request, or `nil`.

**Returns:** `crap.Document?` — The user document (a Lua table containing the user's
fields, including `id`, `email`, and any custom fields), or `nil` if no user is set.

The function returns `nil` in any of these cases:

- Called from `init.lua` (no request context).
- The request is unauthenticated (anonymous public traffic).
- Called outside a hook (no `UserContext` is registered on the Lua VM).

The user is populated from the request's session/JWT and made available to all hooks
that fire for that request (CRUD lifecycle hooks, access control functions, validation
hooks, `before_render`, etc.).

```lua
crap.hooks.register("before_change", function(ctx)
    local user = crap.auth.user()

    if user then
        ctx.data.last_edited_by = user.id
    end

    return ctx
end)
```

```lua
-- Skip a hook for anonymous traffic
crap.hooks.register("after_read", function(ctx)
    local user = crap.auth.user()
    if not user then
        return ctx
    end

    -- ...user-specific personalization...

    return ctx
end)
```

## Notes

- Available in both init.lua and hooks.
- Uses the same Argon2id implementation as the built-in auth system.
- Useful for custom auth strategies or migrating users from external systems.
- For custom authentication logic, use [auth strategies](../authentication/custom-strategies.md) or [auth callbacks](../authentication/custom-strategies.md#auth-callbacks).
