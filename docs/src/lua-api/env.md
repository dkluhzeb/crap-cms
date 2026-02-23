# crap.env

Read-only access to environment variables.

## Functions

### `crap.env.get(key)`

Get the value of an environment variable.

**Parameters:**
- `key` (string) — Environment variable name.

**Returns:** string or nil — The value, or `nil` if the variable is not set.

```lua
local db_url = crap.env.get("DATABASE_URL")
if db_url then
    crap.log.info("DB URL: " .. db_url)
end

-- Common pattern: env with fallback
local port = crap.env.get("PORT") or "3000"
```

## Notes

- Available in both init.lua and hooks.
- Returns `nil` for unset variables (never errors).
- Useful for reading secrets, feature flags, or deployment-specific values without hardcoding them in Lua files.
