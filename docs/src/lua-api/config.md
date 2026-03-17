# crap.config

Read-only access to `crap.toml` configuration values using dot notation.

## Functions

### `crap.config.get(key)`

Get a configuration value by dot-separated key path.

**Parameters:**
- `key` (string) — Dot-separated config key (e.g., `"server.admin_port"`).

**Returns:** any — The value at that key path, or `nil` if the path doesn't exist.

```lua
local port = crap.config.get("server.admin_port")   -- 3000
local host = crap.config.get("server.host")          -- "0.0.0.0"
local dev = crap.config.get("admin.dev_mode")        -- false
local depth = crap.config.get("depth.max_depth")     -- 10
local expiry = crap.config.get("auth.token_expiry")  -- 7200
```

## Available Keys

The config structure mirrors `crap.toml`:

| Key | Type | Default |
|-----|------|---------|
| `server.admin_port` | integer | 3000 |
| `server.grpc_port` | integer | 50051 |
| `server.host` | string | "0.0.0.0" |
| `database.path` | string | "data/crap.db" |
| `admin.dev_mode` | boolean | false |
| `auth.secret` | string | "" |
| `auth.token_expiry` | integer | 7200 |
| `depth.default_depth` | integer | 1 |
| `depth.max_depth` | integer | 10 |
| `upload.max_file_size` | integer | 52428800 |
| `hooks.on_init` | string[] | [] |

## Notes

- Values are a **read-only snapshot** taken at VM creation time. Changes to `crap.toml` after startup won't be reflected until the process restarts.
- Available in both init.lua and hooks.
- Returns `nil` for non-existent keys (never errors).
