# crap.toml

The `crap.toml` file configures the server, database, authentication, and other global settings. All sections and fields are optional — sensible defaults are used when omitted.

If no `crap.toml` file exists in the config directory, all defaults are used. An empty file is also valid — all defaults apply.

## Top-Level Fields

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `crap_version` | string | — | Expected CMS version. If set, a warning is logged on startup when the running binary doesn't match. Supports exact (`"0.1.0"`) or prefix (`"0.1"`) matching. |

## Environment Variable Substitution

String values in `crap.toml` can reference environment variables using `${VAR}` syntax:

```toml
[auth]
secret = "${JWT_SECRET}"

[database]
path = "${DB_PATH:-data/crap.db}"

[email]
smtp_pass = "${SMTP_PASSWORD}"
```

- `${VAR}` — replaced with the value of `VAR`. Startup fails if `VAR` is not set.
- `${VAR:-default}` — replaced with `VAR` if set and non-empty, otherwise uses `default`.

Substitution only applies to string values — `${VAR}` patterns in comments are safely ignored.

This allows keeping secrets out of config files and varying configuration across environments.

## Duration Values

Most time-related fields accept an integer (seconds), a human-readable string with a suffix, or a bare number string:

```toml
# These are all equivalent:
token_expiry = 7200
token_expiry = "7200"
token_expiry = "2h"

# Supported suffixes: s (seconds), m (minutes), h (hours), d (days)
poll_interval = "5s"
login_lockout_seconds = "5m"
auto_purge = "7d"
```

Fields that support this: `token_expiry`, `login_lockout_seconds`, `reset_token_expiry`, `forgot_password_window_seconds`, `max_age`, `poll_interval`, `cron_interval`, `heartbeat_interval`, `auto_purge`, `grpc_rate_limit_window`, `connection_timeout`, `smtp_timeout`, `busy_timeout`, `request_timeout`, `grpc_timeout`.

## File Size Values

File size fields accept **both** an integer (bytes) and a human-readable string:

```toml
# These are equivalent:
max_file_size = 52428800
max_file_size = "50MB"

# Supported suffixes (case-insensitive, 1024-based):
# B (bytes), KB (kilobytes), MB (megabytes), GB (gigabytes)
max_file_size = "500B"
max_file_size = "100KB"
max_file_size = "1GB"
```

Fields that support this: `max_file_size` (global and per-collection), `max_memory`, `http_max_response_bytes`, `grpc_max_message_size`.

## Configuration Validation

`crap.toml` is validated at startup. Fatal validation errors prevent the server from starting with a descriptive error message. Non-fatal issues log warnings.

**Fatal errors:**
- `database.pool_max_size = 0`
- `database.connection_timeout = 0`
- `hooks.vm_pool_size = 0`
- `server.admin_port` or `server.grpc_port` is `0`
- `server.admin_port == server.grpc_port` (ports must be distinct)
- `auth.password_policy.min_length > auth.password_policy.max_length`

**Warnings (server starts but logs a warning):**
- `jobs.max_concurrent = 0` — no jobs will execute
- `auth.secret` is set but shorter than 32 characters
- `depth.max_depth = 0` — all population requests capped to 0

## Full Reference

```toml
# Optional: warn if the running binary doesn't match this version
# crap_version = "0.1.0"

[server]
admin_port = 3000       # Admin UI port
grpc_port = 50051       # gRPC API port
host = "0.0.0.0"        # Bind address
# public_url = "https://cms.example.com"  # Public-facing base URL for generated links
# h2c = false           # Enable HTTP/2 cleartext (for reverse proxies)
# trust_proxy = false   # Trust X-Forwarded-For (enable behind reverse proxy)
# compression = "off"   # "off" (default), "gzip", "br", "all"
# grpc_reflection = false        # Enable gRPC server reflection (default: false)
# grpc_rate_limit_requests = 0   # Per-IP request limit (0 = disabled, recommended: 100)
# grpc_rate_limit_window = 60    # Sliding window in seconds (or "1m")
# grpc_max_message_size = "16MB" # Max gRPC message size (default 16MB)
# request_timeout = "30s"        # Admin HTTP request timeout (none by default)
# grpc_timeout = "30s"           # gRPC request timeout (none by default)

[database]
path = "data/crap.db"   # Relative to config dir, or absolute
pool_max_size = 32       # Max connections in the pool
busy_timeout = "30s"     # SQLite busy timeout (integer ms or "30s", "1m")
connection_timeout = 5   # Pool checkout timeout (seconds or "5s")

[admin]
dev_mode = false         # Reload templates per-request (enable in development)
require_auth = true      # Block admin when no auth collection exists (default: true)
# access = "access.admin_panel"  # Lua function: which users can access the admin UI

# [admin.csp]                    # Content-Security-Policy (enabled by default)
# enabled = true
# script_src = ["'self'", "'unsafe-inline'", "https://unpkg.com"]
# style_src = ["'self'", "'unsafe-inline'", "https://fonts.googleapis.com"]
# font_src = ["'self'", "https://fonts.gstatic.com"]
# img_src = ["'self'", "data:"]
# connect_src = ["'self'"]
# frame_ancestors = ["'none'"]
# form_action = ["'self'"]
# base_uri = ["'self'"]

[auth]
secret = ""              # JWT signing key. Empty = auto-generated and persisted to data/.jwt_secret
token_expiry = "2h"      # Default token expiry (accepts integer seconds or "2h", "30m", etc.)
max_login_attempts = 5   # Failed attempts per email before temporary lockout
max_ip_login_attempts = 20  # Failed attempts per IP before lockout (higher for shared IPs)
login_lockout_seconds = "5m"  # Lockout duration after max attempts
reset_token_expiry = "1h"    # Password reset token expiry
max_forgot_password_attempts = 3   # Forgot-password requests per email before rate limiting
forgot_password_window_seconds = "15m"  # Rate limit window for forgot-password

[auth.password_policy]
min_length = 8              # Minimum password length
max_length = 128            # Maximum password length (DoS protection)
# require_uppercase = false # Require at least one uppercase letter
# require_lowercase = false # Require at least one lowercase letter
# require_digit = false     # Require at least one digit
# require_special = false   # Require at least one special character

[depth]
default_depth = 1        # Default population depth for FindByID (Find always defaults to 0)
max_depth = 10           # Hard cap on population depth (prevents abuse)
# populate_cache = false           # Cross-request populate cache (opt-in)
# populate_cache_max_age_secs = 0  # Periodic cache clear for external DB mutations

[pagination]
default_limit = 20      # Default limit for Find queries (when none is specified)
max_limit = 1000         # Hard cap on limit — requests above this are clamped
# mode = "page"          # "page" (offset) or "cursor" (keyset)

[upload]
max_file_size = "50MB"   # Global max file size (accepts bytes or "50MB", "1GB", etc.)

[email]
smtp_host = ""           # SMTP server hostname. Empty = email disabled (no-op)
smtp_port = 587          # SMTP port (587 for STARTTLS, 465 for TLS, 25/1025 for plain)
smtp_user = ""           # SMTP username
smtp_pass = ""           # SMTP password
smtp_tls = "starttls"    # "starttls" (default), "tls" (implicit TLS), "none" (plain/test)
from_address = "noreply@example.com"  # Sender email address
from_name = "Crap CMS"  # Sender display name
# smtp_timeout = 30     # SMTP connection/send timeout in seconds (or "30s")

[hooks]
on_init = []             # Lua function refs to run at startup (with CRUD access)
# max_depth = 3          # Max hook recursion depth (0 = no hooks from Lua CRUD)
vm_pool_size = 8         # Number of Lua VMs for concurrent hook execution
                         # Default: max(available_parallelism, 4), capped at 32
max_instructions = 10000000  # Max Lua instructions per hook (0 = unlimited)
max_memory = "50MB"          # Max Lua memory per VM (0 = unlimited)
allow_private_networks = false  # Block HTTP requests to private/loopback IPs
http_max_response_bytes = "10MB"  # Max HTTP response body size

[live]
enabled = true           # Enable SSE + gRPC Subscribe for live mutation events
channel_capacity = 1024  # Broadcast channel buffer size
# max_sse_connections = 1000        # Max concurrent SSE connections (0 = unlimited)
# max_subscribe_connections = 1000  # Max concurrent gRPC Subscribe streams (0 = unlimited)

[locale]
default_locale = "en"    # Default locale code
locales = ["en", "de"]   # Supported locales (empty = disabled)
fallback = true          # Fall back to default locale if field is NULL

[jobs]
max_concurrent = 10          # Max concurrent job executions across all queues
poll_interval = "1s"         # How often to poll for pending jobs
cron_interval = "1m"         # How often to check cron schedules
heartbeat_interval = "10s"   # How often running jobs update their heartbeat
auto_purge = "7d"            # Auto-purge completed/failed runs older than this
image_queue_batch_size = 10  # Pending image conversions to process per poll

[access]
default_deny = false     # When true, deny all operations without explicit access functions

[cors]
allowed_origins = []     # Origins allowed for CORS. Empty = CORS disabled (default)
                         # Use ["*"] to allow any origin
allowed_methods = ["GET", "POST", "PUT", "DELETE", "PATCH", "OPTIONS"]
allowed_headers = ["Content-Type", "Authorization"]
exposed_headers = []     # Response headers exposed to the browser
max_age = "1h"   # How long browsers cache preflight results
allow_credentials = false # Allow cookies/Authorization. Cannot use with ["*"] origins

[mcp]
# enabled = false         # Enable MCP server
# http = false            # Mount POST /mcp on admin server
# config_tools = false    # Enable config read/write tools
# api_key = ""            # API key for HTTP transport
# include_collections = [] # Only expose these collections
# exclude_collections = [] # Hide these collections
```

## Section Details

### `[server]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `admin_port` | integer | `3000` | Port for the Axum admin UI |
| `grpc_port` | integer | `50051` | Port for the Tonic gRPC API |
| `host` | string | `"0.0.0.0"` | Bind address for both servers |
| `h2c` | boolean | `false` | Enable HTTP/2 cleartext (h2c). Allows reverse proxies (Caddy, nginx) to speak HTTP/2 to the backend without TLS. Browsers that don't support h2c fall back to HTTP/1.1 on the same port. |
| `trust_proxy` | boolean | `false` | Trust the `X-Forwarded-For` header for client IP extraction on the **admin HTTP server**. **Enable when running behind a reverse proxy** (nginx, Caddy, etc.) so per-IP rate limiting uses the real client IP. When false (default), the TCP socket address is used and XFF is ignored — preventing IP spoofing when exposed directly to the internet. Does not affect the gRPC server, which always uses the TCP peer address from Tonic's `remote_addr()`. |
| `compression` | string | `"off"` | Response compression. `"off"` = disabled (default), `"gzip"` = gzip only, `"br"` = brotli only, `"all"` = gzip + brotli. Most deployments use a reverse proxy (nginx/caddy) for compression, so this is opt-in. |
| `grpc_reflection` | boolean | `false` | Enable gRPC server reflection. Allows clients (e.g., `grpcurl`, Postman) to discover services and methods without a `.proto` file. Disabled by default to hide the API surface from unauthenticated probing. |
| `public_url` | string | — | Public-facing base URL (e.g., `"https://cms.example.com"`). Used for password reset emails and other generated links. If not set, defaults to `http://{host}:{admin_port}`. |
| `grpc_rate_limit_requests` | integer | `0` | Maximum number of gRPC requests per IP within the sliding window. `0` = disabled (default). **Recommended to enable in production** (e.g., `100`). When enabled, requests exceeding the limit receive `ResourceExhausted` status. |
| `grpc_rate_limit_window` | integer/string | `60` (`"1m"`) | Sliding window duration for rate limiting. Accepts seconds (integer) or human-readable (`"1m"`, `"30s"`). |
| `grpc_max_message_size` | integer/string | `16777216` (`"16MB"`) | Maximum gRPC message size in bytes (applies to both send and receive). Tonic's built-in default is 4MB, which can be exceeded by large `Find` responses with deep population. Accepts bytes or file size string (`"16MB"`, `"32MB"`). |
| `request_timeout` | integer/string | — (none) | Admin HTTP request timeout. When set, requests exceeding this duration return `408 Request Timeout`. SSE streams are exempt (handled by shutdown). Accepts seconds or human-readable (`"30s"`, `"5m"`). |
| `grpc_timeout` | integer/string | — (none) | gRPC request timeout. When set, RPCs exceeding this duration return `DEADLINE_EXCEEDED`. Applies to all RPCs including Subscribe streams. Accepts seconds or human-readable (`"30s"`, `"5m"`). |

### `[database]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `path` | string | `"data/crap.db"` | SQLite database path. Relative paths are resolved from the config directory. Absolute paths are used as-is. |
| `pool_max_size` | integer | `32` | Maximum number of connections in the SQLite connection pool. |
| `busy_timeout` | duration | `30000` (`"30s"`) | SQLite busy timeout in milliseconds. Controls how long a connection waits for locks before returning SQLITE_BUSY. Accepts integer ms or human-readable string (`"30s"`, `"1m"`). |
| `connection_timeout` | duration | `5` | Pool checkout timeout in seconds. How long `pool.get()` waits for a free connection before returning an error. |

### `[admin]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `dev_mode` | boolean | `false` | When true, templates are reloaded from disk on every request. The scaffold sets this to `true` for new projects. Set to `false` in production for cached templates. |
| `require_auth` | boolean | `true` | When true and no auth collection exists, the admin panel shows a "Setup Required" page (HTTP 503) instead of being open. Set to `false` for fully open dev mode without authentication. |
| `access` | string | — | Lua function ref (e.g., `"access.admin_panel"`) that gates admin panel access. Called after successful authentication with `{ user }` context. Return `true` to allow, `false`/`nil` to deny (HTTP 403). |
| `csp` | table | *(see below)* | Content-Security-Policy header configuration. See `[admin.csp]`. |

### `[admin.csp]`

Content-Security-Policy header configuration for the admin UI. Each field is a list of CSP sources for the corresponding directive. Theme developers can extend these lists to allow external resources (CDNs, custom fonts, analytics, etc.).

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | boolean | `true` | Enable the CSP header. Set to `false` to disable entirely. |
| `default_src` | string[] | `["'self'"]` | Fallback for any directive not explicitly set. |
| `script_src` | string[] | `["'self'", "'unsafe-inline'", "https://unpkg.com"]` | Allowed script sources. Includes `'unsafe-inline'` for theme bootstrap and CSRF injection scripts. |
| `style_src` | string[] | `["'self'", "'unsafe-inline'", "https://fonts.googleapis.com"]` | Allowed stylesheet sources. Includes `'unsafe-inline'` for Web Component Shadow DOM styles. |
| `font_src` | string[] | `["'self'", "https://fonts.gstatic.com"]` | Allowed font sources. Includes Google Fonts for Material Symbols icons. |
| `img_src` | string[] | `["'self'", "data:"]` | Allowed image sources. Includes `data:` for inline SVGs. |
| `connect_src` | string[] | `["'self'"]` | Allowed targets for `fetch`, XHR, and WebSocket connections. |
| `frame_ancestors` | string[] | `["'none'"]` | Who can embed this page in a frame. `'none'` prevents clickjacking. |
| `form_action` | string[] | `["'self'"]` | Allowed form submission targets. |
| `base_uri` | string[] | `["'self'"]` | Allowed URLs for `<base>` tags. |

**Example: allowing a custom CDN and analytics:**

```toml
[admin.csp]
script_src = ["'self'", "'unsafe-inline'", "https://unpkg.com", "https://cdn.example.com", "https://analytics.example.com"]
style_src = ["'self'", "'unsafe-inline'", "https://fonts.googleapis.com", "https://cdn.example.com"]
font_src = ["'self'", "https://fonts.gstatic.com", "https://cdn.example.com"]
img_src = ["'self'", "data:", "https://cdn.example.com"]
connect_src = ["'self'", "https://analytics.example.com"]
```

### `[auth]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `secret` | string | `""` (empty) | JWT signing secret. If empty, a random secret is auto-generated and **persisted to `data/.jwt_secret`** so tokens survive restarts. Set explicitly if you prefer to manage the secret yourself. |
| `token_expiry` | integer/string | `7200` (`"2h"`) | Default JWT token lifetime. Accepts seconds (integer) or human-readable (`"2h"`, `"30m"`). Can be overridden per auth collection. |
| `max_login_attempts` | integer | `5` | Maximum failed login attempts per email before temporary lockout. |
| `max_ip_login_attempts` | integer | `20` | Maximum failed login attempts per IP before temporary lockout. Higher than per-email to tolerate shared IPs (offices, NAT). Also used as the per-IP threshold for forgot-password requests. |
| `login_lockout_seconds` | integer/string | `300` (`"5m"`) | Duration of lockout after `max_login_attempts` or `max_ip_login_attempts` is reached. Accepts seconds or human-readable. |
| `reset_token_expiry` | integer/string | `3600` (`"1h"`) | Password reset token expiry. The "Forgot password" email link expires after this duration. Accepts seconds or human-readable. |
| `max_forgot_password_attempts` | integer | `3` | Maximum forgot-password requests per email address before rate limiting. Further requests silently return success without sending email. |
| `forgot_password_window_seconds` | integer/string | `900` (`"15m"`) | Rate limit window for forgot-password requests. Also used as the per-IP window for forgot-password rate limiting. Accepts seconds or human-readable. |

### `[auth.password_policy]`

Password strength requirements applied to all password-setting paths (create, update, reset, CLI).

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `min_length` | integer | `8` | Minimum password length in Unicode characters (codepoints). Multi-byte characters (accented letters, CJK, emoji) each count as 1. Must be ≤ `max_length` or the server refuses to start. |
| `max_length` | integer | `128` | Maximum password length in bytes. Prevents DoS via Argon2 on huge inputs. Uses byte count (not characters) to bound hashing cost. |
| `require_uppercase` | boolean | `false` | Require at least one uppercase letter (A-Z). |
| `require_lowercase` | boolean | `false` | Require at least one lowercase letter (a-z). |
| `require_digit` | boolean | `false` | Require at least one digit (0-9). |
| `require_special` | boolean | `false` | Require at least one special (non-alphanumeric) character. |

### `[depth]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `default_depth` | integer | `1` | Default population depth for `FindByID`. `Find` always defaults to `0`. |
| `max_depth` | integer | `10` | Maximum allowed depth for any request. Hard cap to prevent excessive queries. |
| `populate_cache` | boolean | `false` | Enable cross-request populate cache. Caches populated documents in memory, cleared on any write through the API. Improves read performance for repeated deep population. **Opt-in** because external DB modifications can cause stale reads. |
| `populate_cache_max_age_secs` | integer | `0` | Periodic full cache clear interval in seconds. `0` = disabled (only write-through invalidation). Set `> 0` to limit staleness when the database may be modified outside the API. Only used when `populate_cache = true`. |

### `[pagination]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `default_limit` | integer | `20` | Default page size applied to `Find` queries when no `limit` is specified. |
| `max_limit` | integer | `1000` | Hard cap on `limit`. Requests above this value are clamped to `max_limit`. |
| `mode` | string | `"page"` | Pagination mode: `"page"` (offset-based with `page`/`totalPages`) or `"cursor"` (keyset-based with `startCursor`/`endCursor`). In cursor mode, pass `after_cursor` (forward) or `before_cursor` (backward) instead of `page`. |

### `[upload]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `max_file_size` | integer/string | `52428800` (`"50MB"`) | Global maximum file size. Accepts bytes (integer) or human-readable (`"50MB"`, `"1GB"`). Per-collection `max_file_size` overrides this. Also sets the HTTP body limit (with 1MB overhead for multipart encoding). |

### `[email]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `smtp_host` | string | `""` (empty) | SMTP server hostname. **Empty = email disabled** — all send attempts log a warning and return Ok. |
| `smtp_port` | integer | `587` | SMTP port. 587 is the standard STARTTLS port. |
| `smtp_user` | string | `""` | SMTP authentication username. |
| `smtp_pass` | string | `""` | SMTP authentication password. |
| `smtp_tls` | string | `"starttls"` | TLS mode: `"starttls"` (default, port 587), `"tls"` (implicit TLS, port 465), `"none"` (plain, for testing). |
| `from_address` | string | `"noreply@example.com"` | Sender email address for outgoing mail. |
| `from_name` | string | `"Crap CMS"` | Sender display name. |
| `smtp_timeout` | integer/string | `30` | SMTP connection and send timeout in seconds. Accepts integer or duration string (`"30s"`, `"1m"`). |

When configured, email enables password reset ("Forgot password?" link on login), email verification (optional per-collection), and the `crap.email.send()` Lua API.

### `[hooks]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `on_init` | string[] | `[]` | Lua function refs to execute at startup. These run synchronously with CRUD access — failure aborts startup. |
| `max_depth` | integer | `3` | Maximum hook recursion depth. When Lua CRUD in hooks triggers more hooks, this caps the chain. `0` = never run hooks from Lua CRUD. |
| `vm_pool_size` | integer | `max(cpus, 4)` capped at 32 | Number of Lua VMs in the pool for concurrent hook execution. Default is the number of available CPU cores with a floor of 4 and ceiling of 32. |
| `max_instructions` | integer | `10000000` | Maximum Lua instructions per hook invocation. `0` = unlimited. |
| `max_memory` | integer/string | `52428800` (50 MB) | Maximum Lua memory per VM in bytes. Accepts integer or filesize string (`"50MB"`, `"100MB"`). `0` = unlimited. |
| `allow_private_networks` | boolean | `false` | Allow `crap.http.request` to reach private/loopback/link-local IPs. |
| `http_max_response_bytes` | integer/string | `10485760` (10 MB) | Maximum HTTP response body size. Accepts integer or filesize string (`"10MB"`, `"1GB"`). |

### `[live]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | boolean | `true` | Enable live event streaming (SSE + gRPC Subscribe). |
| `channel_capacity` | integer | `1024` | Internal broadcast channel buffer size. Increase if subscribers lag. |
| `max_sse_connections` | integer | `1000` | Maximum concurrent SSE connections. When reached, new connections receive `503 Service Unavailable`. `0` = unlimited. |
| `max_subscribe_connections` | integer | `1000` | Maximum concurrent gRPC Subscribe streams. When reached, new subscriptions receive `UNAVAILABLE` status. `0` = unlimited. |

See [Live Updates](../live-updates/overview.md) for full documentation.

### `[locale]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `default_locale` | string | `"en"` | Default locale code. Content without an explicit locale uses this. |
| `locales` | string[] | `[]` (empty) | Supported locale codes. **Empty = localization disabled.** When empty, all fields behave as before (single value, no locale columns). |
| `fallback` | boolean | `true` | When reading a non-default locale, fall back to the default locale value if the requested locale field is NULL. Uses `COALESCE` in SQL. |

### `[jobs]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `max_concurrent` | integer | `10` | Maximum concurrent job executions across all queues. |
| `poll_interval` | integer/string | `1` (`"1s"`) | How often to poll for pending jobs. Accepts seconds or human-readable. |
| `cron_interval` | integer/string | `60` (`"1m"`) | How often to evaluate cron schedules. Accepts seconds or human-readable. |
| `heartbeat_interval` | integer/string | `10` (`"10s"`) | How often running jobs update their heartbeat. Used to detect stale jobs. Accepts seconds or human-readable. |
| `auto_purge` | integer/string | `"7d"` | Auto-purge completed/failed runs older than this duration. Accepts seconds or human-readable (`"7d"`, `"24h"`, `"30m"`, `"3600"`). Set to `""` (empty string) to disable auto-purge. Absent = 7 days default. |
| `image_queue_batch_size` | integer | `10` | Number of pending image format conversions to claim per scheduler poll cycle. Increase for higher throughput on capable hardware. |

### `[access]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `default_deny` | boolean | `false` | When `true`, collections and globals without an explicit access function deny all operations. When `false` (default), missing access functions allow all operations. |

Enable this to enforce a "secure by default" posture — every collection must explicitly declare its access rules. Without it, collections without access functions are open to any authenticated (or anonymous) user.

### `[cors]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `allowed_origins` | string[] | `[]` (empty) | Origins allowed to make cross-origin requests. **Empty = CORS disabled** (no layer added, default). Use `["*"]` to allow any origin. |
| `allowed_methods` | string[] | `["GET", "POST", "PUT", "DELETE", "PATCH", "OPTIONS"]` | HTTP methods allowed in CORS preflight. |
| `allowed_headers` | string[] | `["Content-Type", "Authorization"]` | Request headers allowed in CORS requests. |
| `exposed_headers` | string[] | `[]` (empty) | Response headers the browser is allowed to access. |
| `max_age` | integer/string | `3600` (`"1h"`) | How long browsers may cache preflight results. Accepts seconds or human-readable. |
| `allow_credentials` | boolean | `false` | Allow credentials (cookies, `Authorization` header). **Cannot be used with `allowed_origins = ["*"]`** — if both are set, credentials are ignored with a warning. |

When CORS is enabled, the layer is added to both the admin UI (Axum) and gRPC API (Tonic) servers. CORS runs before CSRF middleware, so preflight `OPTIONS` requests get CORS headers without triggering CSRF validation.

### `[mcp]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | boolean | `false` | Enable the MCP (Model Context Protocol) server. Required for both stdio and HTTP transports. |
| `http` | boolean | `false` | Mount `POST /mcp` on the admin server for HTTP-based MCP access. |
| `config_tools` | boolean | `false` | Enable config generation tools (`read_config_file`, `write_config_file`, `list_config_files`). Opt-in because they allow filesystem writes. |
| `api_key` | string | `""` (empty) | API key for HTTP transport. **Required** when `http = true` — the server will refuse to start without one. Requests must include `Authorization: Bearer <key>`. |
| `include_collections` | string[] | `[]` (empty) | Only expose these collections via MCP. Empty = all collections. Enforced at both tool listing and execution time. |
| `exclude_collections` | string[] | `[]` (empty) | Hide these collections from MCP. Takes precedence over `include_collections`. Enforced at both tool listing and execution time. |

See [MCP Overview](../mcp/overview.md) for usage details.

When locales are configured, any field with `localized = true` in its Lua definition gets one column per locale (`title__en`, `title__de`) instead of a single `title` column. The API accepts a `locale` parameter on Find, FindByID, Create, Update, GetGlobal, and UpdateGlobal to control which locale to read/write. The admin UI shows a locale selector in the edit sidebar.

**Special locale values:**
- `"all"` — returns all locales as nested objects: `{ title: { en: "Hello", de: "Hallo" } }`
- Any locale code (e.g., `"en"`, `"de"`) — returns flat field names with that locale's values
- Omitted — uses the default locale

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
# require_auth = true
# access = "access.admin_panel"

[auth]
secret = "a-very-long-random-string-for-jwt-signing"
token_expiry = "24h"
max_login_attempts = 10
login_lockout_seconds = "10m"

[depth]
default_depth = 1
max_depth = 5

[upload]
max_file_size = "100MB"

[email]
smtp_host = "smtp.example.com"
smtp_port = 587
smtp_user = "noreply@example.com"
smtp_pass = "your-smtp-password"
from_address = "noreply@example.com"
from_name = "My App"

[hooks]
on_init = ["hooks.seed.run"]
vm_pool_size = 8
max_instructions = 10000000
max_memory = "50MB"
allow_private_networks = false
http_max_response_bytes = "10MB"
```
