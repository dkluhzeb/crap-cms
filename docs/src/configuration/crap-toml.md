# crap.toml

The `crap.toml` file configures the server, database, authentication, and other global settings. All sections and fields are optional — sensible defaults are used when omitted.

If `crap.toml` does not exist in the config directory, all defaults apply.

## Full Reference

```toml
[server]
admin_port = 3000       # Admin UI port
grpc_port = 50051       # gRPC API port
host = "0.0.0.0"        # Bind address

[database]
path = "data/crap.db"   # Relative to config dir, or absolute

[admin]
dev_mode = true          # Reload templates per-request (disable in production)

[auth]
secret = ""              # JWT signing key. Empty = auto-generated (tokens won't survive restarts)
token_expiry = 7200      # Default token expiry in seconds (2 hours)

[depth]
default_depth = 1        # Default population depth for FindByID (Find always defaults to 0)
max_depth = 10           # Hard cap on population depth (prevents abuse)

[upload]
max_file_size = 52428800 # Global max file size in bytes (50 MB)

[hooks]
on_init = []             # Lua function refs to run at startup (with CRUD access)
```

## Section Details

### `[server]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `admin_port` | integer | `3000` | Port for the Axum admin UI |
| `grpc_port` | integer | `50051` | Port for the Tonic gRPC API |
| `host` | string | `"0.0.0.0"` | Bind address for both servers |

### `[database]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `path` | string | `"data/crap.db"` | SQLite database path. Relative paths are resolved from the config directory. Absolute paths are used as-is. |

### `[admin]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `dev_mode` | boolean | `true` | When true, templates are reloaded from disk on every request. Set to `false` in production for cached templates. |

### `[auth]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `secret` | string | `""` (empty) | JWT signing secret. If empty, a random 64-character secret is generated at startup. **Set this in production** so tokens survive restarts. |
| `token_expiry` | integer | `7200` | Default JWT token lifetime in seconds. Can be overridden per auth collection. |

### `[depth]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `default_depth` | integer | `1` | Default population depth for `FindByID`. `Find` always defaults to `0`. |
| `max_depth` | integer | `10` | Maximum allowed depth for any request. Hard cap to prevent excessive queries. |

### `[upload]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `max_file_size` | integer | `52428800` | Global maximum file size in bytes (50 MB). Per-collection `max_file_size` overrides this. |

### `[hooks]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `on_init` | string[] | `[]` | Lua function refs to execute at startup. These run synchronously with CRUD access — failure aborts startup. |

## Example

```toml
[server]
admin_port = 8080
grpc_port = 9090
host = "127.0.0.1"

[database]
path = "/var/lib/crap/production.db"

[admin]
dev_mode = false

[auth]
secret = "a-very-long-random-string-for-jwt-signing"
token_expiry = 86400  # 24 hours

[depth]
default_depth = 1
max_depth = 5

[upload]
max_file_size = 104857600  # 100 MB

[hooks]
on_init = ["hooks.seed.run"]
```
