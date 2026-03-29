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

```lua
-- These work (if set):
crap.env.get("CRAP_API_TOKEN")   -- returns the value
crap.env.get("LUA_PATH")         -- returns the value

-- These always return nil:
crap.env.get("PATH")             -- nil
crap.env.get("HOME")             -- nil
crap.env.get("DATABASE_URL")     -- nil
```

## Notes

- Available in both init.lua and hooks.
- Returns `nil` for unset variables and for variables with disallowed prefixes (never errors).
- Useful for reading secrets, feature flags, or deployment-specific values without hardcoding them in Lua files.
- To pass configuration to hooks, set environment variables with the `CRAP_` prefix (e.g., `CRAP_SMTP_HOST`, `CRAP_WEBHOOK_SECRET`).
