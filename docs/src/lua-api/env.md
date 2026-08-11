# crap.env

Read-only access to environment variables.

## Functions

### `crap.env.get(key)`

Get the value of an environment variable.

**Parameters:**
- `key` (string) — Environment variable name.

**Returns:** string or nil — The value, or `nil` if the variable is not set.

```lua
local db_url = crap.env.get("CRAP_DATABASE_URL")
if db_url then
    crap.log.info("DB URL: " .. db_url)
end

-- Common pattern: env with fallback
local port = crap.env.get("CRAP_PORT") or "3000"
```

## Allowed Prefixes

For security, `crap.env.get()` only allows access to environment variables with specific prefixes:

| Prefix | Purpose |
|--------|---------|
| `CRAP_` | Application-specific variables (e.g., `CRAP_API_KEY`, `CRAP_WEBHOOK_URL`) |
| `LUA_` | Lua-specific variables (e.g., `LUA_PATH`, `LUA_CPATH`) |

All other environment variables (e.g., `PATH`, `HOME`, `DATABASE_URL`, `AWS_SECRET_ACCESS_KEY`) return `nil` regardless of whether they are set. This prevents hooks from accidentally or maliciously reading sensitive system or infrastructure variables.

### The `CRAP_SECRET_*` reservation

Variables prefixed `CRAP_SECRET_` are **hidden from hooks**: reading one
from `crap.env.get()` **errors** rather than returning the value. This
prefix is reserved for config-only secrets. `crap.toml` `${VAR}`
substitution still reads it (that runs at load, before any Lua VM
exists), so you can reference a `CRAP_SECRET_*` var in config while
keeping it unreadable from userland Lua:

```toml
[auth]
secret = "${CRAP_SECRET_JWT}"   # substituted at load
```

Store any secret that must not be reachable from a hook under this
prefix. Other `CRAP_*` variables remain hook-readable, so put
hook-consumed configuration (webhook URLs, feature flags) under a plain
`CRAP_` name.

```lua
-- These work (if set):
crap.env.get("CRAP_API_TOKEN")   -- returns the value
crap.env.get("LUA_PATH")         -- returns the value

-- These always return nil:
crap.env.get("PATH")             -- nil
crap.env.get("HOME")             -- nil
crap.env.get("DATABASE_URL")     -- nil

-- This ERRORS (reserved, config-only):
crap.env.get("CRAP_SECRET_JWT")  -- raises
```

## Notes

- Available in both init.lua and hooks.
- Returns `nil` for unset variables and for variables with disallowed prefixes; **errors** for a `CRAP_SECRET_*` variable (reserved, config-only).
- Useful for reading feature flags or deployment-specific values without hardcoding them in Lua files. For secrets that hooks should never read, use the `CRAP_SECRET_*` prefix and reference them from `crap.toml` instead.
- To pass configuration to hooks, set environment variables with the `CRAP_` prefix (e.g., `CRAP_SMTP_HOST`, `CRAP_WEBHOOK_SECRET`).
