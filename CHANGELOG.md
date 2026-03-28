# Changelog

All notable changes to this project will be documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/).

## [0.1.0-alpha.3] — Unreleased

### Added

- **Soft deletes** — Collections can opt into soft deletes with
  `soft_delete = true`. Deleted documents are moved to trash (`_deleted_at`
  timestamp) instead of being permanently removed. Soft-deleted documents
  are excluded from all reads, counts, and search. The admin UI shows a
  **Trash** tab with restore and permanent-delete buttons, plus an
  **Empty trash** action. Upload files are preserved until hard purge.
  Configurable retention (`soft_delete_retention = "30d"`) auto-purges
  expired documents. Granular permissions: `access.trash` controls
  soft-delete and restore (falls back to `access.update`), while
  `access.delete` controls permanent deletion. Available in admin UI,
  gRPC (`Delete` with `force_hard_delete`, new `Restore` RPC), and Lua
  (`crap.collections.delete/restore` with `forceHardDelete` option).

- **Delete confirmation dialog** — Replaces the old two-step confirmation
  page with a single modal dialog. For soft-delete collections, shows
  "Move to trash" and "Delete permanently" options. For hard-delete
  collections, shows "Delete permanently" only. "Delete permanently"
  and "Empty trash" buttons are hidden when `access.delete` is not
  configured. Upload collections block deletion when other documents
  reference them.

- **Optional timezone support for date fields** — `timezone = true` on a
  date field stores the user's IANA timezone in a companion `_tz` column
  alongside the UTC value. The admin UI shows a timezone dropdown; the
  user enters local time and sees local time on reload (no drift). API
  responses include both `start_date` (UTC) and `start_date_tz` (IANA
  string). Requires `picker_appearance = "dayAndTime"`. Supports localized
  fields, Groups, Rows, Arrays, versioning, and a global
  `[admin] default_timezone` config fallback.

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
  `channel_capacity > 0`, `pagination.default_limit > 0`,
  `pagination.default_limit <= max_limit`, `depth >= 0`,
  `default_locale` in `locales` list, MCP HTTP requires `api_key`, and
  warns on questionable settings (e.g., SMTP configured but `public_url`
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

- **Reference counting for delete protection** — Every collection table
  now has a `_ref_count` column that tracks how many documents reference
  it. Delete protection is O(1) instead of scanning all collections.
  Covers all relationship types: has-one, has-many, polymorphic, localized,
  array sub-fields, and block sub-fields. Globals that hold outgoing
  references also maintain ref counts on their targets. A one-time
  backfill migration computes initial counts from existing data.

### Changed

- **Delete protection expanded to all collections** — Previously only
  upload/media collections were protected from deletion when referenced.
  Now all collections are protected: attempting to delete a document with
  `_ref_count > 0` is blocked. Bulk `delete_many` silently skips
  referenced documents instead of failing.

- **Delete confirmation page uses lazy-loaded details** — The delete
  confirmation page now shows a fast "Referenced by N document(s)"
  summary from the `_ref_count` column. A "Show details" button
  lazy-loads the full back-reference list (which collections/fields
  reference the document) via a new
  `GET /admin/collections/{slug}/{id}/back-references` endpoint.

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

- **XSS via `javascript:` protocol in richtext links** (CRITICAL): Link
  marks in ProseMirror content rendered `href` attributes without URL
  protocol validation. A `javascript:alert('xss')` href executed
  arbitrary code when clicked. Now only allowlisted protocols (`http`,
  `https`, `mailto`, `tel`, `ftp`, relative paths) are rendered; all
  others are replaced with `#`.

- **Unescaped node type in `<crap-node>` tags** (HIGH): Custom node
  `data-type` attribute used `html_escape` (no quote escaping) instead
  of `html_escape_attr`. A crafted node type with quotes could break
  HTML attribute parsing. Fixed in both renderer and validation handler.

- **Session refresh allowed deleted users** (HIGH): The session refresh
  endpoint checked lock status and session version but never verified the
  user document still exists. A deleted user's session could be
  refreshed indefinitely. Now checks user existence first.

- **Locked accounts could reset passwords** (MEDIUM): The password reset
  flow did not check account lock status. A locked user could reset
  their password and regain access. Now rejects reset attempts for
  locked accounts.

- **gRPC reset password used wrong rate limiter** (MEDIUM): The gRPC
  password reset endpoint used `ip_login_limiter` instead of
  `ip_forgot_password_limiter`, allowing rate limit pool pollution
  between login and reset operations.

- **Date string slicing panic on multi-byte UTF-8** (MEDIUM): Date field
  value slicing used `&val[..10]` which panics if the byte offset falls
  within a multi-byte character. Changed to `.get(..10).unwrap_or(val)`.

- **String slicing panics on multi-byte UTF-8** (HIGH): Eight locations
  across the codebase used `find()` + byte-offset slicing (`&s[..pos]`)
  which panics when the offset falls within a multi-byte character.
  Affected: polymorphic ref parsing (3 sites), form bracket parsing,
  CLI key=value parsing, template path splitting, richtext attribute
  extraction, and timestamp normalization. All converted to `split_once`
  or guarded with `is_char_boundary`.

- **gRPC Subscribe connection limit TOCTOU race** (MEDIUM): The
  `fetch_add` + check pattern allowed concurrent requests to exceed the
  configured `max_subscribe_connections`. Replaced with a
  `compare_exchange_weak` CAS loop matching the SSE implementation.

- **`url_decode` garbled multi-byte UTF-8** (HIGH): Percent-encoded multi-byte
  sequences (e.g. `%C3%A9` for `é`, CJK, emoji) were decoded byte-by-byte as
  individual `char`s, producing mojibake. Malformed `%XX` sequences silently
  dropped characters. Rewritten to collect decoded bytes into `Vec<u8>` then
  convert via `String::from_utf8_lossy`; malformed sequences are now preserved
  literally.

- **NaN/Infinity accepted in number fields** (HIGH): Submitting `"NaN"`,
  `"inf"`, or `"-inf"` as a number field value parsed successfully and stored
  non-finite floats in the database. Added `is_finite()` check — non-finite
  values now coerce to `NULL`.

- **Rate limiter bypass via unparseable XFF** (HIGH): When `trust_proxy = true`
  and `X-Forwarded-For` contained a non-IP string, `client_ip()` used the raw
  garbage string as the rate limiter key. Attackers could vary this per-request
  to get unique rate limit buckets. Unparseable XFF now falls back to the TCP
  socket address.

- **SSE connection limit TOCTOU race** (HIGH): The SSE connection counter used
  `fetch_add` + check + `fetch_sub`, allowing a race where concurrent requests
  could exceed the configured `max_sse_connections`. Replaced with a
  `compare_exchange_weak` loop for atomic slot acquisition.

- **JSON template helper `</script>` breakout** (MEDIUM): The `{{{json ...}}}`
  Handlebars helper did not escape `</` in serialized values. A value containing
  `</script>` could break out of a `<script>` block in the admin UI. Now
  replaces `</` with `<\/` after serialization.

- **Pagination offset overflow** (MEDIUM): Extreme `page` values (near
  `i64::MAX`) caused integer overflow in `(page - 1) * limit`. Changed to
  `saturating_mul` to prevent panics.

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

- **MCP HTTP unauthenticated access** (HIGH): When `mcp.http = true` and
  `api_key` was empty, the MCP HTTP endpoint accepted unauthenticated requests
  with full CRUD access (MCP bypasses all access control). The server now
  requires an API key when MCP HTTP is enabled (config validation error at
  startup). The HTTP handler also rejects requests as a defense-in-depth guard.

- **MCP `exclude_collections` bypass** (MEDIUM): `exclude_collections` and
  `include_collections` only filtered the `tools/list` response — an attacker
  who knew a collection slug could call `find_<slug>` directly via
  `tools/call`. Collection filters are now enforced at execution time.

- **Lua `update_many` skipped validation and hooks** (HIGH): The Lua
  `crap.collections.update_many()` function only ran `BeforeChange` hooks and
  discarded their return value. It skipped `BeforeValidate` hooks, field
  validation (`required`, `unique`, custom `validate`), and field-level
  `before_change`/`after_change` hooks. Now runs the full write lifecycle
  matching the single `update` and gRPC `UpdateMany` paths.

- **Lua `update_many` field write access bypass** (MEDIUM): When called with
  `overrideAccess = false`, field-level write access checks were not applied.
  Now strips denied fields before the DB write.

- **IP rate limiter not cleared on successful login** (MEDIUM): The per-IP
  rate limiter was never cleared on successful login (only the per-email
  limiter was). Users behind shared IPs (NAT, VPN) could eventually get
  locked out despite successful logins. Both limiters are now cleared on
  success (admin and gRPC).

- **Lua `delete`/`delete_many` orphaned upload files** (MEDIUM): Deleting
  upload-collection documents via Lua hooks left files on disk. Now cleans up
  upload files after successful deletion, matching the gRPC path.

- **`sanitize_locale` empty string passes in release builds** (HIGH):
  `sanitize_locale` used `debug_assert!` which only fires in debug builds.
  An all-special-character locale string silently produced `""` in release,
  which gets interpolated into SQL as an empty identifier. Now returns
  `Result<String>` with a proper error, propagated through all callers.

- **Non-existent locale silently accepted**: `LocaleContext::from_locale_string`
  accepted any locale code without checking it exists in the config's locale
  list. Requesting a non-existent locale (e.g. `"fr"` when only `"en"` and
  `"de"` are configured) silently created a `Single("fr")` context. Now
  returns `None` for unknown locale codes.

- **Lua table conversion stack overflow** (HIGH): `lua_to_json` and
  `json_to_lua` recursed into nested tables with no depth limit. A deeply
  nested structure (65+ levels) caused stack overflow. Now capped at 64
  levels with a clear error.

- **Mixed-key Lua tables silently lost string keys** (HIGH): A Lua table
  with both integer and string keys (e.g., `{1, 2, name="test"}`) was
  treated as a JSON array, silently dropping string keys. Now detected
  and serialized as a JSON object preserving all keys.

- **Version table index name collision** (HIGH): Version table indexes
  used names like `idx_{slug}_parent_latest` that could collide with
  field-level indexes on fields named `parent_latest`. Namespaced to
  `idx__ver_{slug}_*`.

- **Polymorphic relationship upgrade left stale PRIMARY KEY** (HIGH):
  Upgrading a junction table from non-polymorphic to polymorphic added
  the `related_collection` column but didn't update the PRIMARY KEY
  constraint. Now rebuilds the table with the correct composite PK.

- **Silent NaN/Infinity and number overflow in gRPC conversion** (MEDIUM):
  Non-finite floats silently became `null` and overflowing numbers
  silently became `0.0` in protobuf conversion. Now logs warnings.

- **Event publishing error silently swallowed** (MEDIUM): Collection
  definition lookup failure during event publishing was discarded with
  `.ok()`. Now logs a warning.

- **Sessions not invalidated on password change** (HIGH): After a password
  reset, existing JWTs remained valid until expiry. Added a
  `_session_version` counter to auth tables that increments on password
  change. The version is embedded in JWT claims and checked on every
  authenticated request — stale tokens are rejected immediately.

### Fixed

- **JSON API responses missing `charset=utf-8`**: Upload API `json_error`
  and `json_ok` helpers set `Content-Type: application/json` without
  charset, which could cause encoding issues with older clients. Now
  includes `charset=utf-8`.

- **MCP HTTP errors returned plain text instead of JSON-RPC**: Auth
  failures (missing/invalid API key) and body-too-large errors on the
  MCP HTTP endpoint returned plain text responses. MCP clients expecting
  JSON-RPC 2.0 format couldn't parse these errors. Now returns proper
  `JsonRpcResponse::error` with appropriate error codes.

- **Empty IN/NOT IN filter generated invalid SQL**: `FilterOp::In(vec![])`
  produced `field IN ()` which is invalid SQL. Empty IN now returns `FALSE`
  (`0 = 1`) and empty NOT IN returns `TRUE` (`1 = 1`).

- **Image resize integer overflow on extreme aspect ratios**: Resize
  dimension calculation used unchecked `f64 → u32` cast that could wrap
  on extreme aspect ratios. Now clamped to `u32::MAX`.

- **SVG CSP strengthened**: Added `default-src 'none'` alongside the
  existing `sandbox` directive for defense-in-depth on SVG uploads.

- **Group filter normalization missed layout wrappers**: Filtering on a
  Group field nested inside Row/Tabs/Collapsible failed because
  `normalize_field_name` only checked top-level fields. Now recursively
  searches through transparent layout wrappers.

- **Job retry with no backoff**: Failed jobs were immediately re-queued
  as `pending` with no delay, causing tight retry loops. Now uses
  exponential backoff (`min(2^attempt * 5, 300)` seconds) via a
  `retry_after` column.

- **Populate cache not locale-aware — cross-locale data leakage**: The
  relationship populate cache keyed on `(collection, id)` without locale.
  Two requests for the same document in different locales could return
  cached data from the wrong locale. Cache key now includes locale.

- **JWT secret loss on failed write**: If the auto-generated JWT secret
  could not be persisted to disk (permissions, full disk), the server
  started with an ephemeral secret. On restart, a new secret was
  generated, invalidating all sessions. Now fails to start instead.

- **Config validation gaps**: Added checks for `smtp_port = 0` when SMTP
  host is configured, `request_timeout = 0` / `grpc_timeout = 0` (use
  `None` to disable), and `grpc_rate_limit_window = 0` when rate limiting
  is enabled.

- **Cron `skip_if_running` TOCTOU race**: The check for running jobs and
  the insert of a new job were not atomic. Two scheduler instances could
  both see count=0 and both insert. Now wrapped in
  `transaction_immediate()`.

- **Join field populate with negative depth**: Join field population
  passed `depth - 1` without guarding `depth > 0`, allowing negative
  depth values. Now skipped when depth is exhausted.

- **Hardcoded English strings in UI components**: Drawer close button
  aria-label, confirm dialog fallback text, and toast colors now use
  `t()` translations and CSS custom properties respectively.

- **Card header text overflow**: Long card titles broke flex layout.
  Added `text-overflow: ellipsis` and overflow containment.

- **Cursor encoding error silently dropped**: `cursor.encode().ok()` discarded
  serialization errors, causing pagination to silently break. Now logs the
  error before returning `None`.

- **MCP resources returned empty JSON on serialization failure**: Three
  `unwrap_or_default()` calls in MCP resource endpoints silently produced
  empty strings when schema serialization failed. Now logs the error.

- **Richtext link dialog null dereferences**: Four `querySelector()` calls in
  the link modal's `applyLink()` function accessed `.value`/`.checked`
  without null checks, causing crashes if modal DOM was malformed.

- **Filter builder null dereferences**: `list-settings.js` filter builder
  accessed `.value` on `querySelector()` results without null checks.

- **Stale warning buttons used `onclick` property assignment**:
  `live-events.js` used `btn.onclick =` instead of `addEventListener()`,
  overwriting any existing click handlers.

- **Cursor pagination broken on numeric fields**: Cursor sort values were always
  bound as `TEXT`, so numeric columns compared lexicographically (`"9" > "10"`).
  Number values now bind as `INTEGER`/`REAL` and `NULL` cursors bind as SQL `NULL`
  instead of empty string.

- **Silent "unknown" block type on missing `_block_type`**: Block rows without a
  `_block_type` key silently defaulted to `"unknown"`, masking form parsing bugs
  and persisting unrenderable blocks. Now returns an error.

- **Version snapshot corruption silently lost**: Malformed JSON in version
  snapshots was swallowed via `unwrap_or(Null)`, permanently losing the snapshot
  data with no error. Now propagates the parse error.

- **Double-space labels for group sub-fields**: `to_title_case("seo__title")`
  produced `"Seo  Title"` (double space). Now filters empty segments from
  consecutive underscores, producing `"Seo Title"`.

- **`after_read` hook errors silently swallowed**: Hook failures were logged at
  WARN and the unmodified document was returned, serving stale data with no
  visible indication. Elevated to `error!` with full error chain.

- **Hook non-table return silently ignored**: If a Lua hook returned a string,
  number, or boolean instead of a table (common mistake), the original context
  was used with no feedback. Non-nil non-table returns now log a warning.

- **Form field read errors silently became empty strings**: Multipart form field
  read failures (e.g., truncated uploads) were hidden by `unwrap_or_default()`.
  Now logs the error before falling back.

- **Field name `__` collision with group naming**: Field names containing double
  underscores (e.g., `seo__title`) are now rejected during schema parsing, since
  `__` is reserved as the group field separator in column names.

- **Theme picker crash in restricted storage contexts**: `localStorage` access in
  the theme picker could throw in embedded iframes or with storage policies
  disabled. Wrapped in try/catch.

- **Dirty form / list settings listeners lost on DOM reconnect**: The
  `<crap-dirty-form>` and `<crap-list-settings>` components cleaned up
  document/window listeners in `disconnectedCallback` but did not reset their
  initialization guard, so re-insertion into the DOM left them inert. Guard is
  now reset on disconnect.

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

- **Duplicate IDs in nested array templates**: When adding a parent array row,
  `_replaceIndexInNestedTemplates` replaced **all** `__INDEX__` placeholders —
  including those belonging to child array levels — corrupting nested templates
  so every child row cloned from them got identical hardcoded IDs. Rewritten to
  use targeted replacement based on the parent fieldset's `data-field-name`,
  replacing only the parent-level `__INDEX__` while preserving child-level
  placeholders. Also added nested template reindexing in `_reindexRows` so child
  templates reflect the correct parent index after drag-reorder.

- **Nested array actions fired twice (event bubbling)**: Click events on nested
  `crap-array-field` actions (add/remove/move/duplicate) bubbled up to the
  parent `crap-array-field`, which also handled them — doubling the effect
  (e.g., adding 2 sub-items instead of 1). Added ownership check so each
  component only handles actions belonging to its own level.

- **Nested drag-and-drop events bubbled to parent**: `_onDragStart`,
  `_onDragOver`, and `_onDrop` had no ownership checks. Dragging a nested
  array's row caused both parent and child components to handle the drag,
  potentially moving rows to the wrong container or corrupting indices. Added
  ownership checks for drag handles and container elements.

- **`_getDragAfterElement` selected nested rows**: The drop position calculation
  used `querySelectorAll('.form__array-row:not(...)')` which matched ALL
  descendant rows including those in nested arrays. Changed to
  `:scope > .form__array-row` to only consider direct children.

- **Nested `crap:request-add-block` event fired twice**: The
  `crap:request-add-block` custom event from `crap-block-picker` bubbled to
  parent `crap-array-field` components, causing duplicate block row additions.
  Added ownership check on the event target.

- **Listener accumulation on nested component reconnect**: Row move operations
  (`insertBefore`) triggered `disconnectedCallback`→`connectedCallback` on
  nested `crap-array-field` elements. Since `disconnectedCallback` reset
  `_connected` without removing listeners, each reconnect added duplicate
  handlers via fresh `bind()` calls. Stopped resetting `_connected` so listeners
  survive disconnect/reconnect cycles without accumulation.

- **Duplicated row label watcher skipped**: `_duplicateRow` cloned the row
  including `data-label-init="1"`, causing `_setupRowLabelWatcher` to bail out
  on the clone. Typing in the duplicate's label field never updated the row
  title. Now clears `data-label-init` before setting up the watcher.

- **`_setupBlockRowLabelWatcher` was exact duplicate**: Identical to
  `_setupRowLabelWatcher`. Removed and consolidated all callers to use the
  single method.

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

- **`channel_capacity = 0` startup panic**: Setting `live.channel_capacity = 0`
  in `crap.toml` caused a tokio panic at startup (`broadcast::channel` requires
  capacity > 0). Now caught by config validation with a clear error message.

- **Missing config validation for pagination limits**: `pagination.default_limit`
  and `pagination.max_limit` accepted zero or negative values. Negative
  `default_limit` passed through to SQL `LIMIT`, causing undefined behavior.
  Now validated: both must be > 0, and `default_limit <= max_limit`.

- **`default_locale` not validated against `locales` list**: Setting
  `default_locale = "en"` with `locales = ["de", "fr"]` was silently accepted,
  causing the default locale to have no storage columns. Now errors at startup.

- **Negative depth config accepted**: `depth.default_depth` and `max_depth`
  accepted negative values. Now validated: both must be >= 0.

- **SSE reconnection created duplicate `EventSource`**: If the SSE connection
  dropped and the component was reconnected during the 5-second retry window,
  both the timer callback and `connectedCallback` created new connections.
  Reconnect timer is now tracked and cleared on disconnect.

- **Array field index collision after row removal**: Removing a row and adding
  a new one could produce duplicate indices because `_afterRowChange()` did
  not call `_reindexRows()`. Indices are now resequenced on every row change.

- **Array checkbox/label association broken on new rows**: `_replaceIndexInSubtree`
  did not replace `__INDEX__` in `label[for]` attributes, so newly added array
  rows had non-functional checkbox labels. `_reindexRows` also did not update
  `id` or `label[for]` attributes, breaking label association after drag-reorder.
  Both methods now update all relevant attributes.

- **Web Component event listener accumulation**: Multiple components lacked
  `_connected` guards or reset their guard flag in `disconnectedCallback`,
  causing duplicate event listeners on DOM reconnect (HTMX swaps, drag
  reorder). Affected: `CrapArrayField`, `CrapConfirm`, `CrapTags`,
  `CrapDirtyForm`, `CrapCollapsible`, `CrapBlockPicker`, `CrapTabs`,
  `CrapFocalPoint`, `CrapListSettings`, `CrapUploadPreview`,
  `CrapRelationshipSearch`, and all picker components (`CrapThemePicker`,
  `CrapLocalePicker`, `CrapUiLocalePicker`). Symptoms ranged from
  double-toggling collapsible groups, duplicate block additions, drawer
  opening multiple times, to confirmed form submissions being blocked.
  Added `_connected` guards to all components; stopped resetting the flag
  in `disconnectedCallback`.

- **Relationship search state loss on reconnect**: `CrapRelationshipSearch`
  reset `_initialized` in `disconnectedCallback`, causing a full DOM rebuild
  (`innerHTML = ''`) on reconnect that destroyed selected items and search
  state.

- **Focal point `0` treated as center**: `parseFloat(...) || 0.5` in
  `CrapFocalPoint` treated a legitimate focal-point coordinate of `0` as
  falsy, defaulting it to `0.5` (center). Changed to explicit `Number.isNaN`
  check.

- **Dirty form re-queried form reference in disconnect**:
  `CrapDirtyForm.disconnectedCallback` called `this.querySelector('#edit-form')`
  to remove listeners. If the form element was detached before the wrapper,
  the query could miss it, leaking `input`/`change` listeners. Now stores the
  form reference during `connectedCallback`.

- **Tab keyboard navigation**: `CrapTabs` did not implement WAI-ARIA keyboard
  navigation. Added ArrowLeft/Right, Home/End key handling with proper
  `tabindex` management.

- **Relationship search stale dropdown**: A pending `fetch` from `doSearch()`
  could resolve after `closeDropdown()`, reopening the dropdown. Now
  increments the generation counter on close to invalidate in-flight searches.

- **Block row label watcher duplicate listeners**: `_setupBlockRowLabelWatcher`
  lacked the `labelInit` guard present in `_setupRowLabelWatcher`, allowing
  duplicate `input` listeners on reconnection.

- **Auth page cache-busting**: Login, forgot-password, and reset-password
  pages linked to `/static/styles.css` without the `?v={{crap.build_hash}}`
  cache-busting parameter used by other pages.

- **Missing favicon on standalone pages**: Forgot-password, reset-password,
  auth-required, and admin-denied pages were missing the
  `<link rel="icon">` tag, causing 404s for `/favicon.ico`.

- **gRPC reflection docs misleading**: Documentation implied reflection was
  always available. Clarified that `grpc_reflection = true` must be set.

- **Reset token expiry docs hardcoded**: gRPC docs said tokens expire
  "after 1 hour" instead of referencing the configurable `reset_token_expiry`.

- **`sanitize_locale` empty string invariant**: Added `debug_assert!` to catch
  pathological input (all non-alphanumeric characters) that produces an empty
  locale identifier. Panics in debug builds; documents the invariant.

- **`append_default_value` type mismatch warnings**: Now logs `tracing::warn!`
  when a default value type obviously mismatches the field type (e.g., string
  default on a Number field, bool default on a Text field).

- **Removed dead `FieldHooks::is_empty()`**: Unused `#[allow(dead_code)]`
  method — individual Vec fields are checked directly at all call sites.

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
