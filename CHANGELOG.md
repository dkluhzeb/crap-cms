# Changelog

All notable changes to this project will be documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/).

## [0.1.0-alpha.3] — Unreleased

### Changed

- **Richtext node attrs now use the field system** — `register_node` attrs are now
  defined with `crap.fields.*` factory functions instead of the old `{ name, type }`
  table syntax. Supports all scalar field types (`text`, `number`, `textarea`, `select`,
  `radio`, `checkbox`, `date`, `email`, `json`, `code`). Complex types are rejected at
  registration time. Node edit modals now support `placeholder`, `description`, radio
  groups, date pickers, email inputs, and monospace editors for code/json fields.

- **Full field feature support for richtext node attrs:**
  - Admin display hints: `hidden`, `readonly`, `width`, `step`, `rows`, `language`,
    `min`/`max`, `min_length`/`max_length`, `min_date`/`max_date`, `picker_appearance`
  - Server-side validation: `required`, `validate`, length/numeric/date bounds, email
    format, option validity — errors reference node location (e.g. `content[cta#0].url`)
  - `before_validate` hooks for normalizing attr values before validation
  - Registration-time warnings for features that have no effect on node attrs
    (`unique`, `index`, `localized`, `access`, `before_change`, `after_change`,
    `after_read`, `has_many`, `mcp`, `admin.condition`)

### Added

- **Content-Security-Policy header** — configurable `[admin.csp]` section with
  per-directive source lists (`script_src`, `style_src`, `font_src`, etc.).
  Enabled by default with permissive defaults that cover the built-in admin UI.
  Theme developers can extend the lists to allow external CDNs, fonts, and
  analytics scripts. Set `enabled = false` to disable entirely.

- **SSE connection limiting** — `max_sse_connections` in `[live]` (default:
  1000). Returns `503 Service Unavailable` when the limit is reached. `0` =
  unlimited.

- **gRPC Subscribe connection limiting** — `max_subscribe_connections` in
  `[live]` (default: 1000). Returns `UNAVAILABLE` when the limit is reached.
  `0` = unlimited.

- **Admin HTTP request timeout** — `request_timeout` in `[server]` (optional,
  none by default). Returns `408 Request Timeout` when exceeded. SSE streams
  are exempt. Accepts seconds or human-readable (`"30s"`, `"5m"`).

- **gRPC request timeout** — `grpc_timeout` in `[server]` (optional, none by
  default). Returns `DEADLINE_EXCEEDED` when exceeded. Accepts seconds or
  human-readable.

- **Configurable gRPC message size** — `grpc_max_message_size` in `[server]`
  (default: `"16MB"`). Replaces Tonic's 4MB default, which can be exceeded by
  large `Find` responses with deep relationship population. Accepts bytes or
  human-readable (`"16MB"`, `"32MB"`).

- **IP rate limiting** on auth endpoints (login, forgot-password). Configurable
  per-IP limits with automatic cooldown (`max_ip_login_attempts` in `[auth]`).

- **Reset password rate limiting** — per-IP rate limiting on the reset-password
  endpoint (admin and gRPC) to prevent brute-forcing reset tokens.

- **`trust_proxy` config** (`[server]`) — controls whether `X-Forwarded-For` is
  trusted for client IP extraction. Default: `false` (XFF ignored). Enable when
  running behind a reverse proxy so per-IP rate limiting uses the real client IP.

- **H2C support** (HTTP/2 cleartext) for deployment behind reverse proxies.
  New `[server] h2c` config option.

- **Populate cache cap** (`MAX_POPULATE_CACHE_SIZE = 10,000`) prevents unbounded
  memory growth during read-heavy workloads.

- **Hooks on bulk operations** — `before_change`/`after_change` hooks now fire
  per-document for `UpdateMany`, and `before_delete`/`after_delete` for
  `DeleteMany`. Version snapshots are also created per-document. Opt out with
  `hooks = false` in the request.

- **Startup config validation** — validates port > 0, admin_port != grpc_port,
  and warns on questionable settings (e.g., SMTP configured but `public_url`
  missing).

- **Security headers** on all admin responses: `X-Frame-Options: DENY`,
  `X-Content-Type-Options: nosniff`, `Referrer-Policy`,
  `Permissions-Policy` (camera, microphone, geolocation disabled).

- **`crap.json` namespace** — `crap.json.encode()` / `crap.json.decode()` as
  cleaner aliases for `crap.util.json_encode()` / `crap.util.json_decode()`.
  The old `crap.util.json_*` functions continue to work.

- **Lua type definitions** — `types/crap.lua` provides LuaLS-compatible
  `@class`/`@param`/`@return` annotations for the entire `crap.*` API,
  enabling IDE autocompletion and type checking.

### Changed

- **Scaffold `dev_mode`** defaults to `false` (was `true`). New projects start
  secure by default.

- **Admin templates**: Pagination variables now live exclusively under the
  `pagination` object (e.g. `pagination.prev_url` instead of `prev_url`).
  Templates using the `{{> components/pagination}}` partial work automatically.
  Custom templates that referenced top-level pagination keys (`page`, `per_page`,
  `total`, `total_pages`, `has_prev`, `has_next`, `prev_url`, `next_url`,
  `has_pagination`) must update to use the `pagination.*` prefix. The
  `has_pagination` key has been removed — use `{{#if pagination.has_prev}}`
  / `{{#if pagination.has_next}}` directly. The `pagination` object is always
  present when `with_pagination` is called, even on single-page results.

- **MCP `find` response**: Pagination metadata is now nested under a
  `"pagination"` key instead of being flat in the response object. The response
  shape is now `{ "docs": [...], "pagination": { "totalDocs": ..., ... } }`.

- **Admin templates**: The `items` context key for collection list pages is now
  `docs`, matching the naming used by MCP and gRPC. Update custom templates:
  `{{#if items}}` → `{{#if docs}}`, `{{#each items}}` → `{{#each docs}}`.

- **Upload cleanup guard**: `process_upload` now returns an RAII `CleanupGuard`
  that the caller must `.commit()` after their DB transaction succeeds. Prevents
  orphaned files when the DB write fails after files are already on disk.

- **CORS `max_age_seconds`** renamed to **`max_age`** for consistency with other
  duration fields. Accepts integer seconds or human-readable (`"1h"`, `"30m"`).

- **Scaffold CORS config** — `crap init` now outputs `max_age` instead of the
  old `max_age_seconds` in the commented CORS section.

### Security

- **Content-Security-Policy** (NEW): Admin UI now sends a CSP header by default
  with restrictive `default-src`, `frame-ancestors 'none'`, `form-action 'self'`,
  and `base-uri 'self'`. Inline scripts/styles are allowed via `'unsafe-inline'`
  (required for theme bootstrap, CSRF injection, and Shadow DOM components).

- **X-Forwarded-For bypass** (HIGH): `client_ip()` no longer trusts XFF by
  default. Without `trust_proxy = true`, the TCP socket address is used,
  preventing attackers from spoofing IPs to bypass per-IP rate limits.

- **Shared rate limiters** (MEDIUM): Admin and gRPC servers now share the same
  `LoginRateLimiter` instances, preventing attackers from doubling their attempt
  budget by targeting both servers.

- **Richtext node attr XSS** (HIGH): Custom node attribute values were rendered
  unescaped into `innerHTML` in the richtext editor modal and inline node
  display. Values containing `<`, `>`, `"`, `'`, or `&` could break the DOM
  or enable stored XSS. All interpolated values are now HTML-escaped. The
  server-side `before_validate` hook output is also escaped when
  reconstructing `<crap-node>` tags.

- **SSRF DNS rebinding closed** (HIGH): `crap.http.request()` now resolves DNS
  once, validates against the SSRF policy, and pins the validated IP via
  `reqwest::ClientBuilder::resolve()`. No second DNS lookup occurs at connect
  time — eliminates the TOCTOU DNS rebinding gap that existed with ureq.
  Redirects are individually resolved, validated, and pinned before following.

- **Migration concurrency** — `sync_all` now uses `transaction_immediate()` to
  serialize concurrent DDL operations via SQLite's write lock + `busy_timeout`,
  preventing schema corruption from concurrent startups.

- **Version uniqueness constraint** — UNIQUE index on `(_parent, _version)` in
  versions tables prevents duplicate version numbers from race conditions.

- **SSRF IPv6-mapped IPv4 bypass** (HIGH): `is_private_ip()` did not check
  IPv6-mapped IPv4 addresses (`::ffff:127.0.0.1`, `::ffff:10.0.0.1`, etc.).
  These bypassed the SSRF filter entirely. Now extracts the inner v4 address
  via `to_ipv4_mapped()` and re-checks it.

- **Field access fail-open on VM pool exhaustion** (HIGH): `check_field_read_access`
  and `check_field_write_access` returned empty denied lists (= allow all) when
  the Lua VM pool failed to acquire. Changed to fail-closed — all
  access-controlled fields are denied when the pool is unavailable.

- **Rate limiter IPv6 bypass** (MEDIUM): With `trust_proxy = true`, the raw
  `X-Forwarded-For` string was used as the rate limiter key. Different IPv6
  representations of the same address (e.g., `2001:db8::1` vs
  `2001:0db8:0:0:0:0:0:1`) got separate buckets. Now parsed as `IpAddr` and
  re-serialized to canonical form.

- **Logout CSRF** (LOW): The `/admin/logout` endpoint accepted GET requests,
  allowing forced logout via `<img src="/admin/logout">`. Now POST-only.

- **Sensitive form Debug redaction** (LOW): `LoginForm` and `ResetPasswordForm`
  now redact passwords and tokens in their `Debug` output, preventing
  accidental exposure in logs.

- **UNIQUE constraint error leaks schema** (MEDIUM): gRPC error messages for
  unique constraint violations included internal table names (e.g.,
  `UNIQUE constraint failed: users.email`). Now sanitized to show only the
  column name.

### Fixed

- **Page metadata stomping**: `with_pagination` no longer overwrites the `page`
  context object (title, type, title_name) with the pagination page number.

- **Admin socket address extraction**: The non-H2C admin server was not using
  `into_make_service_with_connect_info::<SocketAddr>()`, so `ConnectInfo`
  extraction failed at runtime — broke `trust_proxy` and per-IP rate limiting.

- **Relationship link URLs**: The join field template appended `/edit` to
  relationship item URLs (e.g. `/admin/collections/tags/123/edit` instead of
  `/admin/collections/tags/123`), causing 404s when clicking linked items.

- **Relationship search label association**: The `<crap-relationship-search>`
  input was missing an `id` attribute, breaking `<label for="...">` matching.

- **Relationship search null-safety**: `JSON.parse(getAttribute('selected'))`
  could return `null` instead of an array, causing a TypeError when iterating.

- **Join field label element**: The join field template used a `<label>` without
  a `for` attribute — changed to `<span class="form__label">` for correct
  semantics.

- **Richtext `<crap-node>` tag parsing**: The parser searched for `</crap-node>`
  before `/>`, so a self-closing tag before a full closing tag consumed too
  much content. Rewritten to find whichever closing pattern comes first.

- **Richtext node attr validation in nested fields**: Richtext fields inside
  array or blocks containers did not have their custom node attributes
  validated. Added recursive field walking to find all richtext fields.

- **Richtext node attr `required` skipped for drafts**: Required validation on
  custom node attributes fired even for drafts, blocking users from saving
  incomplete work.

- **Form validation double-submit**: The `<crap-validate-form>` component's
  `_runValidation()` could fire concurrently on rapid double-click. Added a
  guard flag to prevent concurrent validation requests.

- **Verification token expiry silent failure**: `find_by_verification_token`
  silently defaulted expiry to 0 on data corruption, making all tokens appear
  expired. Now uses proper error propagation (consistent with reset tokens).

- **`DeleteMany` file deletion before commit**: Upload files were deleted from
  disk before the DB transaction committed. If the commit failed, documents
  would reference missing files. Files are now deleted after successful commit.

- **Heading level not lower-bounded**: A ProseMirror document with
  `"level": 0` produced invalid `<h0>`. Now clamped to 1-6.

- **Job retry stale heartbeat**: `fail_job` with retry did not clear
  `heartbeat_at`, leaving a stale timestamp from the failed run.

- **`__INDEX__` partial replacement in array templates**: `replace()` only
  replaced the first `__INDEX__` occurrence per attribute. Changed to
  `replaceAll()` so nested templates work correctly.

- **`getConfirmDialog()` null crash**: `dirty-form.js` called `.prompt()` on
  null when no `<crap-confirm-dialog>` exists. Added null guard with safe
  fallback.

- **Password max_length error message**: Said "characters" but checked bytes.
  Fixed to say "bytes" (intentional for Argon2 DoS prevention).

- **Richtext modals inaccessible**: Link and node edit modals were plain
  `<div>` overlays without focus trapping, Escape handling, or ARIA roles.
  Converted to native `<dialog>` elements with `aria-labelledby`.

- **Relationship search dropdown invisible to screen readers**: Added
  `role="combobox"`, `aria-expanded`, `role="listbox"`, and `role="option"`.

- **Hardcoded English in UI components**: Replaced hardcoded "Cancel",
  "Confirm", "OK", "Search..." strings with `t()` translations in confirm,
  richtext, and relationship-search components.

### Internal

- Unified pagination output into `PaginationResult` struct + builder in
  `db::query`. All 4 entry points (gRPC, MCP, Admin, Lua) use a single
  computation path with thin format-specific adapters.

- Unified pagination input validation via `PaginationCtx`, reducing
  `validate_find_pagination` call signatures from 7 parameters to 4.

- Removed `pagination_builder.rs` (gRPC) and `find_pagination_input_builder.rs`
  (Lua) — consolidated into `db::query::pagination_result`.

- Removed 4 duplicated `resolve_sort()` implementations (now 1).

- Extracted CSRF and auth middleware from monolithic `server.rs`.

- Split oversized modules into focused submodules: auth handlers, field context
  enrichment, document types, hook context, MCP tool dispatch, scaffold hooks.

- Harmonized test macros and module imports across codebase.

- Extracted `get_text`/`get_opt_text` helpers in image queue code, replacing
  repeated match-and-clone blocks.

- Replaced `ureq` with `reqwest` (blocking + rustls-tls) for the Lua HTTP
  client. Enables DNS pinning via `ClientBuilder::resolve()` and reuses
  existing rustls/hyper/tokio transitive deps.
