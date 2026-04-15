# Changelog

All notable changes to this project will be documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/).

## [0.1.0-alpha.5] — Unreleased

### Added

- **`crap-cms update` built-in version manager** — nvm-style CLI for
  managing installed versions of the binary. Subcommands: `check`,
  `list`, `install <version>`, `use <version>`, `uninstall <version>`,
  `where`. Bare `crap-cms update` installs the latest release and
  switches to it. Versions live under `~/.local/share/crap-cms/versions/`;
  the `current` symlink flip is atomic (safe to switch while `serve` is
  running). Release assets are verified against `SHA256SUMS` before
  install. Distro-managed paths (`/usr/`, `/opt/`, `/nix/`) refuse
  self-update with a pointer at the system package manager; `--force`
  overrides. On `crap-cms serve` startup, a one-line notice prints
  when the cached update-check (24h TTL) shows a newer release is
  available — silenceable via `[update] check_on_startup = false` in
  `crap.toml`. Windows self-update (`install`/`use`) is not supported
  in this release — the version store uses symlinks. Windows users
  should download new releases manually; `check`/`list`/`where` still
  work.

- **Official shell installer** at `scripts/install.sh` — auto-detects
  platform (Linux x86_64 / aarch64), downloads the matching asset,
  verifies SHA256, lays out the nvm-style version store under
  `~/.local/share/crap-cms/`, wires up a shim at `~/.local/bin/crap-cms`,
  and prints a PATH hint if needed. Install via
  `curl -fsSL https://raw.githubusercontent.com/dkluhzeb/crap-cms/main/scripts/install.sh | bash`.

- **Top-level `hidden` field flag** — new `hidden = true` on
  `crap.FieldDefinition` strips a field from all read responses (gRPC,
  Lua, MCP, admin JSON, REST) and skips it in the admin form. Writes
  are not stripped — internal hooks/Lua can still write the column.
  This separates the two concerns that `admin.hidden` was overloaded
  to express: `admin.hidden` now controls admin-form rendering only
  (data still returned by the API, matching PayloadCMS's `hidden`
  semantic), while top-level `hidden` is the strict "do not return
  this anywhere" flag. Both flags are independent and composable.

- **`[live] transport = "redis"` for cross-node event fan-out** — new
  config key that pipes live-update mutation events and user-invalidation
  signals through Redis pub/sub instead of the default in-process
  `tokio::sync::broadcast` channel. Required for multi-node deployments
  so subscribers on any node see events published by any other node.
  Reuses `[cache] redis_url` (single source of truth); requires
  `--features redis` at build time. With the default `transport =
  "memory"`, a write on node A still only reaches subscribers connected
  to node A — fine for single-node or sticky-load-balanced setups, not
  for round-robin.

- **Rust typegen proto conversion** — `crap-cms typegen -l rs --proto <module>`
  generates `generated_proto.rs` with `from_document()` impls that extract
  fields directly from `prost_types::Struct` — no JSON intermediate, no
  serde deserialization. Depends only on `prost_types`, not `tonic`.
  Sub-types (array rows) get `from_struct()` methods. Handles all field
  types: text, number, checkbox, relationships, arrays with sub-fields,
  uploads, selects. Layout wrappers (Row, Collapsible, Tabs) are
  transparently promoted.

- **gRPC trash query** — `Find` and `FindByID` requests now accept an
  optional `trash` parameter. When `trash = true`, only soft-deleted
  documents are returned (sorted by `_deleted_at` descending by default).
  Uses `access.trash` permission (falls back to `access.update`) instead
  of `access.read`. Requires `soft_delete = true` on the collection.
  Previously, soft-deleted documents were only accessible through the
  admin UI.

- **Admin access harmonization** — The admin UI now delegates all access
  checks and field stripping to the service layer instead of duplicating
  them. Read-denied fields are completely hidden from edit forms (previously
  they rendered as empty form fields, leaking field existence). Removed
  redundant `strip_denied_fields` from admin handlers. The collection list,
  edit form (collections + globals), and delete confirmation page all go
  through the service layer with proper `ServiceError::AccessDenied`
  handling.

- **Configurable session cookie SameSite attribute** — new `[auth]
  session_cookie_samesite` key in `crap.toml` accepts `"lax"` (default),
  `"strict"`, or `"none"` (reserved; currently falls back to `Lax` with
  a runtime warning). Set to `"strict"` for hardened CSRF protection at
  the cost of breaking cross-site navigation (clicks from emails, external
  links, etc. will require re-login). The CSRF cookie itself remains
  hard-coded to `SameSite=Strict` regardless of this setting.

- **`crap.crypto.constant_time_eq(a, b)`** — new Lua-side helper that
  compares two strings in time independent of where (or whether) they
  differ, backed by the `subtle` crate. Required for verifying HMAC tags,
  signatures, or any secret value — using Lua's `==` operator on HMAC
  strings is timing-attack-vulnerable. The `crap.crypto.hmac_sha256`
  docs now point to this helper as the only correct verification path.

### Changed

- **User-invalidation signals now fire from the service layer** —
  `ServiceContext` carries an optional `invalidation_transport`; when
  set, `service::auth::lock_user` and `service::write::delete_document_core`
  (for hard-delete of auth collections) publish a user-invalidation
  signal so any active live-update streams owned by that user are torn
  down. Wired through admin handlers, gRPC handlers (lock, delete,
  delete_many, upload delete), MCP delete tool, empty-trash, and Lua
  CRUD (`crap.collections.delete` / `delete_many`). Lua VMs receive the
  transport via `LuaInvalidationTransport` app-data set at `HookRunner`
  build time. The previously-duplicated handler-side publishers in
  admin + gRPC handlers have since been removed — the service-layer
  chokepoint is now the single source of invalidation publishes.

- **Cross-request populate cache dedup** — `FindDocumentsInput` and
  `FindByIdInput` gained an optional `singleflight: Option<
  SharedPopulateSingleflight>` field plumbed through
  `PostProcessOpts` into `post_process`. gRPC find / find_by_id
  handlers thread the process-wide `Arc<Singleflight>` from
  `ContentServiceDeps` / `AdminState`. Lua CRUD paths read the shared
  singleflight from `LuaPopulateSingleflight` app-data. Combined with
  the `override_access` guardrail (see Fixed section), this closes
  the last gap on SEC-F: concurrent requests across the process
  dedupe populate cache misses, while override-access fetches stay
  isolated. MCP tools hardcode `override_access = true` so the
  guardrail always bypasses their threading — intentionally skipped.

- **Docs + LuaLS annotations for `list_versions` / `restore_version`** —
  both functions are now documented in `docs/src/lua-api/collections.md`
  and typed in `types/crap.lua` (plus `example/types/crap.lua`) with the
  `crap.VersionSummary` shape and their `overrideAccess` opt. See the
  corresponding Fixed entry for the behaviour change.

- **BREAKING: filters on system columns (`_*`) are now rejected** —
  User-supplied `where` filters targeting field paths starting with `_`
  (e.g. `_deleted_at`, `_status`, `_ref_count`, `_password_hash`,
  `_locked`) now error with `InvalidArgument` / `HookError` instead of
  silently ANDing against automatically-injected exclusions or falling
  through. Applies to gRPC, Lua, admin URL query params, and MCP. Use
  the typed request flags (`trash = true`, `draft = true`) to access the
  data those columns represent. Previously, such filters could silently
  produce empty results (for drafts-enabled collections without the
  `draft` flag) or — in Lua bulk ops and gRPC bulk — bypass validation
  entirely. Validation moved into the service layer so all surfaces
  enforce the same rule. The allow-list for service-internal injection
  (`_status = "published"` when filtering to non-drafts, `_deleted_at
  EXISTS` when listing trash) is applied post-validation.

- **BREAKING: `AccessResult::Constrained` filter tables from write
  access hooks now enforce row-level matches** — An access hook for
  `update` / `delete` / `undelete` / `unpublish` that returns a filter
  table (e.g. `return { author_id = ctx.user.id }`) now causes the
  target row to be checked against those filters; the operation is
  denied if the row does not match. Previously the filter was silently
  dropped and the write proceeded unchecked — operators writing the
  natural "users can only modify their own rows" idiom were getting a
  no-op. This restores the intuitive semantic across reads + writes.
  On `create`, filter tables are now rejected with a clear error
  (`create` has no target row to match); use boolean returns with
  explicit `ctx.data` checks instead. On globals (single-row) and jobs
  (trigger-only), filter tables are likewise rejected with an
  operator-facing error. Version `restore` enforces against the parent
  document id.

- **BREAKING: relationship population omits soft-deleted / missing
  targets** — At `depth >= 1`, a has-one relationship whose target is
  soft-deleted or absent now resolves to `null` instead of leaking the
  raw ID string. Has-many relationships drop soft-deleted / absent
  entries from the array. Cycle-protection paths, malformed polymorphic
  refs, and unknown-collection refs still keep the original string.
  Applies to both single-doc and batch population, polymorphic and
  non-polymorphic.

- **Slow / lagged subscribers are dropped (SEC-D)** — Live-update
  streams (gRPC Subscribe and admin SSE) now drop a subscriber when a
  per-event send takes longer than `subscriber_send_timeout_ms` (new
  `[live]` key, default `1000`). Subscribers that fall further behind
  than `channel_capacity` are also dropped on their next read; the
  previous behavior of holding lagged subscribers open with warnings
  masked silent event loss. Healthy subscribers are unaffected; dropped
  clients see a closed stream and must reconnect.

- **BREAKING: filter comparison operators are field-type-aware** —
  Comparison operators (`greater_than`, `less_than`, `gt`, `lt`, etc.)
  now bind their values as the field's actual SQL type (`INTEGER` /
  `REAL` / `TEXT`) instead of always `TEXT`. Number fields correctly
  compare numerically (previous lexicographic `"1000" < "50"` ordering
  is gone). Checkbox fields accept `"true"`/`"false"`/`"1"`/`"0"` and
  bind as integer. Date fields stay `TEXT` (ISO strings compare
  lexicographically). Text-only operators (`contains`, `starts_with`,
  `regex`) remain text. Invalid numeric inputs fall back to text with a
  runtime warning rather than panicking.

- **`search_documents` now mirrors `find_documents` draft-inclusion
  semantics** — `SearchDocumentsInput` gained an `include_drafts:
  bool` field. When `false` (default) on a drafts-enabled collection,
  the service injects `_status = "published"` so only published rows
  are returned — matching `find_documents`. The admin relationship
  picker passes `include_drafts = true` so operators can link to
  work-in-progress content. Previously search hard-coded a "permit
  `_status` Constrained filter" flag but never actually injected,
  producing inconsistent behaviour between find and search.

- **Cache stampede fix — singleflight on populate** — relationship
  population deduplicates concurrent cache-miss fetches for the same
  `(collection, id, locale)` key. Previously N concurrent requests
  for the same target each independently ran `find_by_id`; now the
  first arriver runs the query and later arrivers block on a shared
  slot, collapsing N DB hits to 1 under thundering-herd load.
  Dashmap-backed, sync-blocking. See the follow-up plumbing for
  cross-request dedup in the Changed section.

### Fixed

- **Admin upload edit page renders the image preview and focal-point
  selector again.** The admin access harmonization in this release had
  extended the service-layer field stripping to also strip every field
  marked `admin.hidden = true`, conflating "don't render in the admin
  form" with "remove from API output". Upload's auto-injected meta
  fields (`url`, `mime_type`, `filesize`, `width`, `height`,
  `focal_x`, `focal_y`, per-size variants) were marked `admin.hidden`
  to keep them out of the form, so the service stripped them — and the
  admin's upload preview widget (which also reads them via the service
  layer) got nothing to render. The two concerns are now split:
  `admin.hidden` is admin-form rendering only; the new top-level
  `hidden` is the API-stripping flag (see Added). Upload meta fields
  are restored to all API responses, fixing the missing image preview
  and unblocking gRPC/Lua/MCP consumers that need them.

- **[SECURITY] SEC-G Join field population bypassed target-collection
  read access** — `populate_join_docs` was running raw `query::find`
  on the joined collection, skipping its `access.read` hook. A user
  allowed to read `post` but denied `author` reads could still see
  `author` data by inspecting the post's join field. Populate now
  checks the target collection's read access via a new
  `JoinAccessCheck` trait: `Denied` → empty array; `Constrained(...)`
  → validated and merged into the subquery; `Allowed` → proceeds. The
  guard is wired from `post_process` for every find/find_by_id result.

- **[SECURITY] Shared populate cache + singleflight leak across
  `override_access = true` boundaries** — Lua CRUD paths and MCP tools
  can set `override_access = true` to bypass access hooks. With the
  cross-request singleflight share landed in this release, a bypass
  fetch could write documents into the shared cache that another
  user's request would then read without their own access
  re-evaluation. Added a single-chokepoint guardrail at
  `service::read::post_process`: when `ctx.override_access == true`,
  both the populate cache and the singleflight are forced to `None`
  regardless of what the input carries. Override-access fetches still
  deduplicate within their own call via a fresh per-call singleflight,
  but never write to or read from shared state.

- **[SECURITY] Live-update streams not torn down on lock / hard-delete
  (SEC-E)** — When a user was locked or hard-deleted via the service
  layer, their existing gRPC Subscribe and admin SSE streams kept
  receiving events with the original snapshotted access until the
  client disconnected on its own. Both surfaces now publish a
  per-user invalidation that closes affected streams immediately with
  `PermissionDenied`; the client must reconnect with a fresh token.
  Anonymous subscribers are not affected.

- **[SECURITY] Lua `list_versions` / `restore_version` bypassed
  collection access** — both functions hardcoded `override_access =
  true`, silently bypassing the collection's `read` / `update` access
  hooks. Now opt-in via `opts.overrideAccess` with a default of
  `false`, matching every other Lua CRUD method. Lua callers respect
  the configured access rules by default; trusted internal code
  (jobs, migrations) can still opt in explicitly.

- **`admin.access` gate not enforced at login** — Users who failed the
  `admin.access` check could still log in and receive a session cookie,
  only to see a 403 on every subsequent page. The access gate is now
  checked in the login handler before issuing the session. Denied users
  see the 403 immediately at login without a cookie being set.

- **ref_count race: dangling reference after concurrent hard-delete** —
  When a write incremented the reference count on a target document that
  had been hard-deleted concurrently, the increment silently failed
  (0 rows affected) and the caller's transaction committed with a
  dangling reference. Now the increment produces a hard error and the
  caller's transaction rolls back. Decrement-on-missing remains a
  tolerated no-op (the target is already gone; nothing to decrement).

- **Custom auth strategy errors silently swallowed** — If a custom
  authentication strategy hook returned an error (DB outage, bad config,
  Lua panic), the login flow silently fell through to the next strategy
  with no log output. Errors are now logged at `ERROR` level with the
  strategy reference and collection slug, then iteration continues.

- **[SECURITY] Email header injection via CRLF in `crap.email.send`** —
  `subject`, `to`, `from`, `cc`, `bcc`, and `reply_to` values are now
  rejected if they contain `\r`, `\n`, or NUL. Previously, a Lua hook
  that interpolated user-controlled data into `subject` could inject
  arbitrary SMTP headers (e.g., `subject = user .. "\r\nBcc: attacker"`
  silently BCC'd the attacker on every mail). The same validation is
  applied at the queued-email insertion point as defense-in-depth.

- **[SECURITY] HTTP SSRF blocklist no longer leaks internal IPs** —
  When `crap.http.request` is blocked by the SSRF policy, the Lua error
  is now a generic "Target resolves to a blocked address; see server
  logs for details". The resolved IP + blocklist class continue to be
  logged at `warn!` level for operators. Previously the error message
  named the IP, which allowed a caller to enumerate internal topology
  via the error channel.

- **MCP HTTP `api_key` empty behavior clarified** — when `mcp.http =
  true` but `api_key` is empty, the server still starts and registers
  the route, but every request to `POST /mcp` is rejected with 401. The
  previous docs claimed the server would refuse to start — that was
  wrong. The per-request check is still defense-in-depth; operators
  should verify the key is set before enabling HTTP.

- **Lua `crap.collections.delete` ignored `forceHardDelete` on
  soft-delete collections** — the option was parsed but never flipped
  `def.soft_delete = false` before calling the service layer, so
  `forceHardDelete = true` silently soft-deleted rows regardless.
  Fixed to mirror the existing pattern in gRPC single/bulk delete
  and admin empty-trash.

- **Configuration parser silently accepted unknown TOML keys** —
  config structs lacked `#[serde(deny_unknown_fields)]`, so typos
  like `[servr]` or `admin_prot = 3000` passed silently and operators
  would spend hours debugging "why isn't my setting applying". Added
  `deny_unknown_fields` to 20 config structs across `src/config/`;
  startup now fails fast on unrecognised keys with an error that
  names the offending key.

- **Parser integer overflow in filesize / duration / trash-purge
  inputs** — `parse_filesize_string` / `parse_duration_string` /
  `parse_older_than` multiplied without checked arithmetic; absurd
  inputs (e.g. `"10000000GB"`, `"99999999999999999999d"`) silently
  overflowed to small or negative values, changing pool sizes,
  timeouts, or purge windows. All three now use `checked_mul` and
  return a clear error on overflow.

- **Field-definition parsing silently accepted duplicate field
  names** — two fields with the same name at the same nesting level
  produced a single column in the generated DDL (the second
  overwrote the first). Parse-time validation now errors with the
  offending name; the check flattens through transparent layout
  wrappers (`Row`, `Collapsible`, `Tabs`) so a sibling field and a
  nested-in-Row field with the same name also collide.

- **Field-config `get_bool` helper silently defaulted on wrong type** —
  a Lua typo like `required = "yes"` (string) parsed as `false`
  instead of erroring. Now returns `LuaResult<bool>` with a clear
  type-mismatch error naming the key and the offending type.

- **Hook / access references validated at startup, not at first
  call** — misspelled refs like `hooks.article.auto_slug` used to
  surface only when a user triggered the corresponding request.
  Startup now walks every collection + global + field-level
  `hooks.*` / `access.*` string and fails fast with a line-by-line
  report of unresolved refs. Job handlers, auth strategies,
  richtext attribute hooks, and dynamic `crap.hooks.register`
  registrations are intentionally out of scope (they have separate
  resolvers or are runtime-dynamic).

- **`crap-cms user create` accepted malformed email addresses** —
  the CLI wrote whatever string the operator supplied into the auth
  collection, breaking downstream password-reset and email-verify
  flows. Now validates format via the same helper used by the
  `email` field type.

- **Config file world-readable warning** — on startup, if `crap.toml`
  contains a non-empty secret (`auth.secret`, `email.smtp_pass`,
  `upload.s3.secret_key`) AND the file's Unix permissions allow
  world-read or world-write, a `warn!` is emitted recommending
  `chmod 600`. Windows: skipped.

- **Null-byte injection in text / textarea / email fields** — user
  input containing `\0` reached SQLite TEXT storage and broke
  downstream display / log handling. Text, textarea, and email
  coercion paths now reject `\0` with a clear per-field error
  naming the offending field.

- **Locale-suffix field-name collision detection** — a literal field
  named `title__en` defined while `en` is an enabled locale would
  collide with the generated localized column for `title`. Startup
  now walks every field (including nested groups / blocks / tabs)
  against the configured locales and fails fast with a clear error.

- **`crap-cms backup` errored mid-run on read-only output dir** —
  the backup started `VACUUM INTO` then failed on the manifest
  write, leaving a partial backup the operator had to clean up. Now
  probes the output directory with a temp file before any long-
  running work; fails early with a clear message.

- **`SIGTERM` shutdown exit code** — the detached-mode serve process
  called `std::process::exit(0)` unconditionally after cleanup, so
  Kubernetes / systemd saw "success" even when WAL checkpoint or
  pool-get failed. Shutdown cleanup now collects errors and the
  process exits with `1` when any cleanup step failed.

- **Version restore silently dropped unknown snapshot keys** — if a
  collection field was deleted after a version was created,
  restoring that version inserted the snapshot without warning about
  the dropped keys. Now emits a `warn!` per unknown key naming the
  collection, version, and key — silent-drop behavior preserved
  so the restore still succeeds, just with visibility.

- **Retention auto-purge ran on every node without dedup** — with
  multiple scheduler instances (multi-server), the soft-delete
  retention purge fired on each node. Now claims a cron window via
  `try_claim_cron_window` (the same mechanism cron jobs use) so only
  one node runs the purge per window.

- **`_ref_count` could double-increment on has-many with duplicate
  IDs** — `extract_has_many_refs` walked the raw JSON input array
  without deduplication, so `tags = ["a", "a", "b"]` incremented
  target `a`'s ref_count twice before the junction-table UNIQUE
  constraint rejected the second row. Now dedupes the ID list first;
  `collect_has_many_refs` also uses `SELECT DISTINCT` as
  defense-in-depth against any pre-existing dirty junction rows.

- **Localized filter on array sub-field routed to the wrong column** —
  a dot-notation filter like `links.label` where `label` is a
  localized sub-field inside an array field did not route to the
  `_locale`-suffixed column and did not add a `_locale = ?`
  constraint to the EXISTS subquery. A locale-scoped filter under
  `Single("de")` matched documents whose value only appeared in `en`.
  Now threaded through `resolve_filter` → `build_subquery_sql`:
  `ResolvedFilter::Subquery` carries a `locale_constraint` that the
  SQL builder appends when set.

### Documentation

- `crap.auth.user()` now documented in `lua-api/auth.md` with return
  shape, nil conditions, and usage examples.
- `before_broadcast` and `before_render` hook events now documented in
  `hooks/lifecycle-events.md` with fire sites, context shapes, return
  value semantics, and examples.
- Decompression-bomb protection (100-megapixel hard limit for image
  uploads) documented in `uploads/image-processing.md`.
- Filter-operator docs rewritten to reflect the field-type-aware
  coercion landed in this release (previously claimed all values were
  coerced to strings).
- HTTP TLS verification (always on, no opt-out) noted in
  `lua-api/http.md`.
- JSON integer-precision caveat (>2^53 loses precision) + recursion
  depth limits noted in `lua-api/json.md`.
- `crap.config` snapshot-per-VM lifecycle clarified in
  `lua-api/config.md`.
- Custom `richtext.register_node` render functions explicitly
  documented as NOT sanitized — operators must escape interpolated
  user data themselves. Added safe / unsafe pattern examples.
- Plugin load order (collections → globals → jobs → init.lua,
  alphabetical within each) explicitly documented in
  `plugins/overview.md`.
- Field-level access denial is silent (no client-facing error) —
  documented in `access-control/field-level.md`.
- Job retry backoff schedule (exponential, capped at 5 min) documented
  in `jobs/overview.md`.
- **Missing gRPC RPCs documented** — `Validate`, `LockAccount`,
  `UnlockAccount`, `VerifyAccount`, and `UnverifyAccount` are now
  covered in `grpc-api/rpcs.md` with request/response shapes,
  `grpcurl` examples, and access requirements. They were defined in
  `proto/content.proto` and live in the running server but were absent
  from the public reference.
- **`live` metadata-mode hook overhead claim corrected** — The live
  updates overview previously claimed `metadata` mode had "zero hook
  overhead". In reality, `before_broadcast` (and the `live` filter
  function, when configured) still run; only the per-subscriber
  `after_read` hooks and field-level read-access stripping on the
  event payload are skipped. Documentation now reflects this.

- **Plugin load order clarification** — documentation now explicitly
  describes the file load order (`collections/` → `globals/` →
  `jobs/` → `init.lua`, all alphabetical within each kind) and the fact
  that plugin `require()` order in `init.lua` is operator-controlled.

- **Job retry backoff documented** — the exponential backoff formula
  (`min(2^(attempt - 1) * 5, 300)` seconds, capped at 5 minutes) is now
  visible in the docs instead of being a runtime surprise.

- **Multi-node file storage corrected** — `deployment/multi-server.md`
  previously listed shared filesystems (NFS / EFS) as a viable option
  for multi-node file storage. They are **not supported** — `storage =
  "local"` assumes a single writer and the code is not tested against
  networked-filesystem fsync / locking semantics. S3-compatible object
  storage (AWS S3, MinIO, Cloudflare R2, Backblaze B2, etc.) is the
  only supported multi-node option.

- **Multi-node rate limiting promoted to required** — shared Redis
  rate limits were previously framed as "recommended for performance".
  They are now documented as a **security requirement**: without them,
  per-IP login rate limits are per-node counters, and an attacker who
  round-robins across nodes multiplies their allowance by the node
  count (e.g. a 5-attempt limit across 3 nodes becomes 15 attempts).

- **Multi-node live updates rewritten** — `deployment/multi-server.md`
  now documents both `transport = "memory"` (default, single-node or
  sticky-LB) and `transport = "redis"` (cross-node fan-out), with the
  trade-offs for each. Cross-links to `live-updates/overview.md`.

- **Load-balancer stickiness requirements documented** — gRPC Subscribe
  / Admin SSE streams are long-lived and benefit from sticky sessions;
  regular HTTP / gRPC unary calls can round-robin freely. Even with
  `transport = "redis"`, reconnects to a different node lose the
  in-flight subscription context and the client must re-subscribe.

- **PostgreSQL backend visibility** — `database/overview.md` now leads
  with both SQLite and PostgreSQL as first-class backends instead of
  treating PostgreSQL as a footnote. Feature parity (FTS, schema sync,
  migrations, ref_count, soft delete) is called out explicitly.

- **Redis auth / TLS documented** — `internals/cache.md` now describes
  how to encode credentials and TLS into the Redis URL (`redis://user:
  pass@host`, `rediss://` for TLS, ACL user syntax for Redis 6+).
  There is no separate `[cache] password` or `[cache] tls` key.

- **Single-server log path + rotation documented** —
  `deployment/single-server.md` now explains that `--detach` auto-
  enables file logging (since the child has no terminal), gives the
  default log location (`<config_dir>/data/logs/`), rotation policy
  (daily, 30-file retention), and how to read logs (`crap-cms logs`
  or tail the files). Notes `--json` for structured output.

- **Cache stampede known-limitation note** — `internals/cache.md`
  documents the cache-miss coalescing behaviour operators should
  expect now that singleflight is active: cache-miss load on the
  same key under heavy concurrency collapses to a single DB query;
  later arrivers block on a shared slot. Also documents the
  override-access isolation invariant.

## [0.1.0-alpha.4] — 2026-04-11

### Changed

- **BREAKING: `default_deny` now defaults to `true`** — Collections and
  globals without explicit access functions now **deny all operations** by
  default. This is a secure-by-default change. Previously, missing access
  functions allowed all operations (`default_deny = false`). To restore
  the old behavior, set `default_deny = false` in `[access]` in
  `crap.toml`. Every collection and global in production should have
  explicit access rules defined.

- **Invalid locale now returns an error** — API requests (gRPC, Lua CRUD)
  with an invalid locale code now receive `INVALID_ARGUMENT` /
  `RuntimeError` instead of silently falling back to the default locale.
  Valid locale codes are those listed in `[locale] locales` in
  `crap.toml`, plus the special value `"all"`. Passing no locale still
  defaults to the default locale.

### Added

- **MCP locale support** — `find` and `find_by_id` MCP tools now accept
  an optional `locale` parameter for querying locale-specific data,
  matching the gRPC API's locale support.

- **Per-collection ref count backfill** — The `_ref_count` backfill
  migration now tracks which collections have been backfilled
  individually. Adding a new collection to the config no longer requires
  manually resetting the `ref_count_backfilled` flag — the backfill runs
  automatically for newly added collections on the next startup.

- **Event delivery modes** — per-collection `live` setting now supports a
  `mode` field (`"metadata"` or `"full"`) that controls what data events
  carry. `metadata` (default) sends only event metadata (sequence,
  operation, collection, document_id) with zero hook overhead — clients
  re-fetch via `FindByID` if needed. `full` mode sends complete document
  data processed through `after_read` hooks and field-level access
  stripping, matching the exact same data a `Find` call returns. Configure
  per collection: `live = { mode = "full" }`. Global default configurable
  via `[live] default_mode` in `crap.toml`.

- **Event stream access control** — SSE and gRPC Subscribe streams now
  enforce the same access rules as normal read operations:
  - Collection-level access (cached at connection time)
  - Row-level constrained access (in-memory filter evaluation per event,
    using the same constraint filters as `Find` SQL WHERE clauses)
  - Field-level read access stripping (in `full` mode, per subscriber)
  - `after_read` hooks (in `full` mode, per subscriber)

- **SSE events now include `data` field** — SSE mutation events now carry
  document data (respecting the collection's delivery mode and access
  control). Previously SSE sent metadata only. This enables custom admin
  UI themes to use real-time document data without re-fetching.

- **In-memory filter evaluation** — new `matches_constraints()` function
  evaluates `FilterClause` types against `HashMap<String, Value>` data
  in-memory. Supports all filter operators (Equals, NotEquals, Contains,
  Like, GreaterThan/LessThan, In/NotIn, Exists/NotExists, Or groups).
  Used by event streams for row-level access control without DB queries.

- **BREAKING: "Restore" renamed to "Undelete" for trash operations** —
  The operation that un-deletes a soft-deleted document is now called
  "undelete" everywhere to distinguish it from "restore version" (which
  reverts a document to a previous snapshot). Affected APIs:
  - gRPC: `rpc Restore` → `rpc Undelete`, `RestoreRequest` → `UndeleteRequest`,
    `RestoreResponse` → `UndeleteResponse`
  - Lua: `crap.collections.restore()` → `crap.collections.undelete()`
  - Admin URL: `/admin/collections/{slug}/{id}/restore` →
    `/admin/collections/{slug}/{id}/undelete`
  - Version restore operations (`RestoreVersion`, `restore_collection_version`,
    etc.) are unchanged.

- **Service layer unification** — all database operation flows now go through
  a shared service layer (`src/service/`), ensuring consistent access control,
  field-level permissions, validation, hydration, and error handling across
  all 4 API surfaces (admin, gRPC, MCP, Lua hooks). Key additions:
  - `ServiceError` with 12 typed variants replacing string-based error matching
  - `WriteHooks::check_access` / `field_write_denied` / `field_read_denied`
    for unified access control inside service operations
  - `service::auth` module for authentication, password reset, email verification
  - `service::version_ops` for version restore/list
  - `service::document_info` for ref counts and back-references
  - `service::user_settings` for per-user preferences
  - `service::jobs` for job queue/run operations with access control
  - `service::upload` for file upload orchestration
  - Write operations now hydrate + strip read-denied fields before returning

- **ServiceContext API harmonization** — every service function now follows
  `fn operation(ctx: &ServiceContext, input)` with a unified calling
  environment. `ServiceContext` carries connection (pool or direct),
  hooks, user identity, slug, and definition. Eliminates all
  `#[allow(clippy::too_many_arguments)]` from the service layer, all
  `_with_conn` variants, and all loose parameter passing. Dedicated input
  structs (`FindDocumentsInput`, `CountDocumentsInput`, `WriteInput`,
  `QueueJobInput`, etc.) carry operation-specific data.

- **Unified pagination** — all multi-result service functions now return
  `PaginatedResult<T>` with docs, total count, and computed pagination
  metadata (page or cursor mode). Pagination logic is built inside the
  service layer — callers no longer duplicate `PaginationResult`
  construction. Affected: `find_documents`, `search_documents`,
  `list_versions`, `list_job_runs`.

### Fixed

- **BREAKING: `admin.hidden` fields now stripped from API responses** —
  Fields with `admin.hidden = true` are no longer returned in gRPC, MCP,
  or Lua API responses. Previously, `hidden` only affected admin form
  rendering. This aligns with PayloadCMS behavior where hidden fields are
  excluded from all responses. Upload metadata fields (`url`,
  `mime_type`, `width`, `height`, `filesize`, `filename`) that were
  auto-injected as `hidden` are affected — if your frontend relies on
  these in API responses, remove `admin.hidden` from the field definition
  or stop marking them hidden in your upload config.

- **Group subfield access stripping** — read-denied fields inside groups
  (e.g., `address.city` with read access denied) are now correctly
  stripped from API responses after hydration. Previously, group subfields
  stored as `address__city` were stripped before hydration but became
  nested `{"address": {"city": ...}}` after hydration, bypassing the
  strip. `Document::strip_fields()` now handles `__`-separated paths at
  any nesting depth.

- **Missing field stripping** — `undelete_document`, `restore_version`
  (collection and global), `search_documents`, and `find_version_by_id`
  now strip read-denied and hidden fields before returning. Previously
  these functions returned unstripped documents.

- **Version snapshot access control** — `find_version_by_id` now checks
  read access and strips denied fields from the snapshot JSON. Previously
  version snapshots were returned without access checks.

- **Redundant proto-level stripping removed** — gRPC handlers no longer
  perform a second pass of field stripping at the protobuf level. The
  service layer handles all field access control, eliminating redundant
  Lua VM access checks and unnecessary transaction opens per response.

- **Surface parity** — all API surfaces now expose a consistent set of operations:
  - MCP: added `undelete`, `count`, `unpublish`, `list_versions`, `restore_version` tools
  - Lua: added `crap.collections.unpublish()`, `list_versions()`, `restore_version()`,
    `ref_count()`, `validate()` functions
  - gRPC: added `Validate`, `LockAccount`, `UnlockAccount`, `VerifyAccount`,
    `UnverifyAccount` RPCs
  - Lua access API: `crap.auth.user()`, `crap.access.check()`,
    `crap.access.field_read_denied()`, `crap.access.field_write_denied()`
  - Lua `crap.jobs.queue()` now checks job access control

- **Module restructuring** — service layer and all surfaces restructured into
  consistent subdirectory hierarchy matching domain concerns:
  - `service/` — 11 subdirectories: types, hooks, read, write, collection,
    globals, persist, versions, jobs, plus auth, upload, user_settings, document_info
  - `api/service/` renamed to `api/handlers/` to avoid confusion with service layer
  - MCP tools split into `collection/{read,write}/`, `globals/`, `schema/`
  - Lua CRUD split into `collection/{read,write,bulk,versions}/`, `globals/`, `jobs/`
  - Admin `forms.rs` (912 lines) → `forms/`; `query_utils.rs` (518 lines) → `query/`
  - gRPC `convert.rs` (881 lines) → `convert/` with document, data, filters, schema

- **Code quality** — namespaced macro calls replaced with top-level imports
  across service + all surfaces (tracing, anyhow, nanoid). Removed unused
  `rpassword` dependency.

- **Internal code quality refactoring** — large files split into focused
  modules following one-handler-per-file and no-logic-in-mod.rs rules.
  Key restructurings:
  - `admin/handlers/shared.rs` → `shared/` module (access, document,
    locale, pagination, response, versions)
  - `admin/server.rs` → extracted `auth_middleware.rs` and `mcp_handler.rs`
  - `admin/templates/mod.rs` → extracted `registry.rs`
  - `api/service/schema_ops.rs` → split into `globals/`, `schema/`,
    `subscribe.rs`, `collection/versions/`, `jobs/`
  - `api/service/collection/{read,write,bulk}.rs` → split into module
    folders with one handler per file
  - `api/service/auth.rs` → split into per-handler files
  - `api/upload.rs` → split into `upload/` module with shared helpers
  - Extracted shared helpers: `evaluate_condition_results`,
    `extract_doc_status`, `load_version_with_missing_relations`,
    `publish_mutation_event`, `strip_read_denied_proto_fields`
  - Eliminated duplicated access check, event publishing, and field
    stripping patterns across admin and API handlers
  - Added `FindQueryBuilder` for `FindQuery` (9 fields, was using
    manual field assignment)

- **Optional PostgreSQL backend** — Crap CMS now supports PostgreSQL as
  an alternative database backend, available via the `postgres` Cargo
  feature flag. SQLite remains the default and recommended backend for
  most deployments.

  **Why SQLite is the default:** Crap CMS is designed for simplicity.
  SQLite requires zero infrastructure — no database server, no
  connection strings, no Docker, no backups to configure. The entire
  database is a single file you can copy, move, or version-control.
  For the vast majority of CMS deployments (content sites, editorial
  teams, headless API backends), SQLite handles thousands of concurrent
  readers and hundreds of writes per second — more than enough.

  **When to consider PostgreSQL:** Multi-server deployments where
  multiple instances need to share a database, or workloads with 50+
  simultaneous writers (rare for a CMS). PostgreSQL also provides
  better read performance under extreme concurrency (50+ concurrent
  requests) due to MVCC.

  **Build & configure:**
  ```bash
  cargo build --features postgres       # both backends
  cargo build --no-default-features --features postgres  # PG only
  ```
  ```toml
  [database]
  backend = "postgres"
  url = "host=localhost user=crap dbname=crap_cms"
  ```

  The `sqlite` and `postgres` feature flags are independent — both can
  be compiled in and switched at runtime via `crap.toml`. The `r2d2`
  dependency is now optional (only pulled with `sqlite`). PostgreSQL
  uses `deadpool-postgres` with `tokio-postgres` for async-native
  connection pooling.

  Postgres-specific implementation details:
  - Full-text search uses `tsvector`/`tsquery` with GIN indexes
    (SQLite uses FTS5)
  - Timestamps stored as ISO 8601 TEXT (matching SQLite behavior)
  - `SET timezone = 'UTC'` enforced on every connection
  - DDL automatically adjusts `INTEGER` to `BIGINT` via dedicated
    `execute_ddl`/`execute_batch_ddl` methods
  - Connection recycling uses `DISCARD ALL` for clean state
  - `VACUUM INTO` not supported (use `pg_dump` for backups)

- **Storage backend abstraction** — Upload file storage is now pluggable
  via a `StorageBackend` trait with three implementations:

  - **`local`** (default) — Local filesystem, identical to previous
    behavior. Zero config, files in `{config_dir}/uploads/`.
  - **`s3`** (feature-flagged) — S3-compatible storage for multi-server
    deployments. Works with AWS S3, MinIO, Cloudflare R2, Backblaze B2,
    DigitalOcean Spaces. Enable with `--features s3-storage`.
  - **`custom`** — Delegates storage operations to user-provided Lua
    functions via `crap.storage.register()`. For exotic providers
    (Azure Blob, GCS, custom APIs) without adding SDK dependencies.

  The entire upload pipeline (upload, serve, resize, delete, deferred
  image conversion) now goes through the storage trait. File serving
  uses `tower_http::ServeFile` for local storage (Range, ETag,
  conditional GET) and streams from the backend for non-local storage.

  ```toml
  [upload]
  storage = "s3"

  [upload.s3]
  bucket = "my-uploads"
  region = "us-east-1"
  endpoint = "http://minio.example.com:9000"
  access_key = "${AWS_ACCESS_KEY}"
  secret_key = "${AWS_SECRET_KEY}"
  path_style = true
  ```

- **Auth redesign: TokenProvider + PasswordProvider + strategy chain** —
  Authentication infrastructure is now cleanly separated:

  - **`TokenProvider` trait** — JWT token creation/validation.
    Default: `JwtTokenProvider`. Rarely swapped.
  - **`PasswordProvider` trait** — Argon2id password hashing/verification.
    Default: `Argon2PasswordProvider`. Rarely swapped.
  - **Strategy chain** — `local` (email+password) is the built-in
    strategy. Per-collection Lua strategies are tried as fallback
    when local auth fails or is disabled (`disable_local = true`).
    No monolithic "auth provider" — authentication is orchestration
    in handlers, not a trait.

  This design supports OAuth2, Cloudflare Access, Active Directory,
  API keys, and any custom auth via **Lua strategies** — without
  the binary needing provider-specific code.

- **Built-in email MFA** — Auth collections can enable email-based
  multi-factor authentication:

  ```lua
  auth = {
      mfa = "email",  -- sends 6-digit code after password verification
  }
  ```

  After password/strategy authentication succeeds, a 6-digit code is
  emailed to the user. The admin UI shows a code input form; the user
  enters the code to complete login. Codes expire after 5 minutes and
  are single-use.

- **Auth callback route** — New catch-all route
  `GET/POST /admin/auth/callback/{name}` dispatches to Lua hooks,
  enabling OAuth2/OIDC callback flows implemented entirely in Lua:

  ```lua
  -- hooks/auth_callback/google.lua
  function M.google(ctx)
      local code = ctx.headers["_query_code"]
      local tokens = exchange_code(code)
      local userinfo = get_userinfo(tokens.access_token)
      local users = crap.find("users", { where = { email = userinfo.email } })
      if #users > 0 then return users[1] end
      return crap.create("users", { email = userinfo.email })
  end
  ```

- **Multi-server scheduler safety** — Job queue is now safe for
  multi-server deployments:

  - **Cron dedup** — New `_crap_cron_fired` table prevents cron jobs
    from double-firing when multiple servers run the scheduler. Uses
    an atomic upsert to claim each cron window.
  - **Atomic job claiming (Postgres)** — Uses `FOR UPDATE SKIP LOCKED`
    for lock-free atomic claiming. Workers skip rows being claimed by
    other workers. Per-slug concurrency limits are enforced inside the
    query (not in-memory).
  - **Atomic job claiming (SQLite)** — Claim operations now run inside
    an IMMEDIATE transaction, serializing concurrent workers. Per-slug
    concurrency counts are read from the DB inside the transaction.

- **Rate limit backend abstraction** — Login and gRPC rate limiters now
  support pluggable backends via a `RateLimitBackend` trait:

  - **`memory`** (default) — In-process sliding window counters. Same
    behavior as before.
  - **`redis`** (feature-flagged) — Shared rate limits across servers
    using Redis sorted sets. Requires `--features redis`.
  - **`none`** — Rate limiting disabled.

  Multi-server deployments should use `redis` to prevent attackers from
  bypassing rate limits by hitting different servers.

  ```toml
  [auth]
  rate_limit_backend = "redis"
  # rate_limit_redis_url defaults to cache.redis_url if empty
  rate_limit_prefix = "crap:rl:"
  ```

- **Cache backend abstraction** — The cross-request populate cache is now
  pluggable via a `CacheBackend` trait with four implementations:

  - **`memory`** (default) — In-memory DashMap with configurable soft
    entry cap. Good for single-server deployments.
  - **`redis`** (feature-flagged) — Shared Redis cache for multi-server
    deployments. Enable with `--features redis`. Uses key prefixing for
    namespace isolation.
  - **`none`** — No-op cache. Disables cross-request caching entirely.
  - **`custom`** — Lua-delegated cache backend (planned, not yet
    implemented — uses `none` as placeholder).

  **Breaking:** The `depth.populate_cache` and
  `depth.populate_cache_max_age_secs` config options have been replaced
  by a new `[cache]` section. Migration:
  - `populate_cache = true` → `[cache] backend = "memory"`
  - `populate_cache = false` → `[cache] backend = "none"`
  - `populate_cache_max_age_secs = 60` → `max_age_secs = 60`

  ```toml
  [cache]
  backend = "memory"       # "memory", "redis", "none", "custom"
  max_entries = 10000      # soft cap for memory backend
  max_age_secs = 60        # periodic full clear (0 = disabled)
  # redis_url = "redis://127.0.0.1:6379"
  # prefix = "crap:"
  ```

- **Email provider abstraction** — Email sending is now pluggable
  via an `EmailProvider` trait with four implementations:

  - **`smtp`** (default) — SMTP via `lettre`, identical to previous
    behavior. Falls back to `log` provider if `smtp_host` is empty.
  - **`webhook`** — HTTP POST to any URL. Works with SendGrid,
    Mailgun, Resend, or any API that accepts JSON email payloads.
    Configure with `webhook_url` and `webhook_headers`.
  - **`log`** — Logs emails to tracing instead of sending. Useful
    for development and testing.
  - **`custom`** — Delegates to a Lua function registered via
    `crap.email.register({ send = function(opts) ... end })`.

  ```toml
  [email]
  provider = "webhook"
  webhook_url = "https://api.sendgrid.com/v3/mail/send"
  webhook_headers = { Authorization = "Bearer ${SENDGRID_API_KEY}" }
  from_address = "noreply@example.com"
  ```

- **`crap-cms work` standalone worker command** — New top-level command
  that runs a dedicated job worker without HTTP/gRPC servers. Supports
  `--queues` (filter by queue name), `--concurrency` (override max
  concurrent jobs), `--no-cron` (skip cron scheduling), and
  `--detach`/`--stop`/`--restart`/`--status` for background management.
  Enables multi-server deployments where app servers run
  `serve --no-scheduler` and dedicated workers process jobs.

- **Queued email delivery with retries** — New `crap.email.queue(opts)`
  Lua API queues emails as jobs for async delivery with automatic
  retries on failure. Uses the existing job system with exponential
  backoff (5s, 10s, 20s, ..., max 300s). Configurable via
  `queue_retries` (default 3), `queue_name` (default `"email"`), and
  `queue_timeout` (default 30s) in `[email]` config. System email jobs
  (`_system_email`) execute directly in Rust without Lua VM overhead.
  `crap.email.send()` remains available for immediate blocking delivery.

### Fixed

- **Access control bypass in bulk delete and empty trash** — The gRPC
  `DeleteMany` handler and the admin "empty trash" handler created
  `RunnerWriteHooks` without `.with_conn(&tx)`, causing all access
  checks inside `delete_document_core` to short-circuit to `Allowed`.
  Any authenticated user could bulk-delete or permanently purge trashed
  documents regardless of configured permissions. Now both paths pass
  the transaction connection to WriteHooks.

- **Version restore missing access control in service layer** — The
  `restore_collection_version` and `restore_global_version` service
  functions did not check update access. The gRPC handler had its own
  check, but admin and MCP handlers did not, allowing any authenticated
  admin user to restore any version. Access check now lives in the
  service layer, enforced for all callers.

- **Ref count race on Postgres** — Under Postgres's `READ COMMITTED`
  isolation, a concurrent create and delete could race: the delete reads
  `_ref_count = 0` while the create's increment is still in flight,
  allowing deletion of a document that is about to be referenced. Fixed
  by acquiring `SELECT ... FOR UPDATE` row locks on referenced targets
  **before** any writes (INSERT/UPDATE), and on the document's own row
  before checking `_ref_count` in the delete path. This serializes
  concurrent create+delete on the same target row. On SQLite this is a
  no-op — `BEGIN IMMEDIATE` already serializes all write transactions.

- **Potential panics in CLI commands** — Several CLI code paths used
  infallible indexing or `.expect()` that could panic on edge cases:
  `trash.rs` used `HashMap[key]` instead of `.get()` (panics if
  collection removed between validation and access); `work.rs` used
  `.unwrap()` on PID conversion (panics if PID > i32::MAX);
  `user/helpers.rs` used `.expect()` on user selection index. All
  replaced with proper error propagation.

- **Rate limiter mutex poisoning could crash server** — The in-memory
  rate limiter used `.expect()` on `Mutex::lock()`, which panics if the
  mutex is poisoned. Now recovers from poison via `unwrap_or_else`.

- **Broadcast stream lag silently ignored** — SSE and gRPC Subscribe
  streams logged subscriber lag at `warn` level (or not at all for SSE)
  with no actionable guidance. Upgraded to `error` with a message
  recommending `[live] channel_capacity` increase.

- **Timestamp expiry overflow** — JWT token expiry was computed as
  `timestamp as u64 + expiry` without overflow protection. Now uses
  `.max(0) as u64` and `saturating_add()` in all 4 production code
  paths (gRPC login, admin login, MFA pending token, session refresh).

- **Invalid locale silently accepted** — `LocaleContext::from_locale_string`
  returned `None` for both "localization disabled" and "invalid locale
  code", making it impossible for callers to distinguish the two cases.
  Invalid locales now produce an error. Affects all API surfaces (gRPC,
  Lua hooks). Admin UI form submissions gracefully fall back to the
  default locale (locales are validated upstream from cookies).

- **Job pagination offset unbounded** — `ListJobRuns` accepted arbitrary
  `offset` values including negative numbers. Now clamped to `>= 0`.

- **MCP tools missing locale** — MCP `find` and `find_by_id` tools did
  not support the `locale` parameter, unlike their gRPC counterparts.
  Claude (via MCP) could not query locale-specific data.

- **Ref count backfill skipped for new collections** — The one-time
  `_ref_count` backfill was gated by a global flag in `_crap_meta`. If a
  new collection was added after the initial backfill, its documents'
  incoming reference counts were never computed. Now tracked
  per-collection.

- **README field type count** — README stated "14 field types" but the
  actual count is 20 (text, number, textarea, richtext, select, radio,
  checkbox, date, email, json, code, relationship, upload, array, group,
  blocks, row, collapsible, tabs, join).

- **`[live] default_mode` missing from docs** — The `default_mode`
  configuration option was documented in the live-updates overview but
  missing from the `crap-toml.md` configuration reference table. Now
  listed in both the example block and the reference table.

- **Event stream data leak** — gRPC Subscribe previously sent full
  document data without applying field-level read access checks. Events
  now respect the same field access rules as `Find` and `FindByID`.

- **Upload API event data ordering** — upload create/update handlers now
  strip field-level read-denied fields before publishing events, ensuring
  event data never contains fields the publishing user's access would deny.

- **Upload file deletion broken on localized collections** — The
  delete cleanup path used `LocaleConfig::default()` (empty) instead
  of the actual locale config when loading the document for file URL
  extraction. On collections with localized fields, the SELECT query
  referenced bare column names (`caption`) instead of locale-suffixed
  ones (`caption__en`), causing the query to fail. Upload files were
  silently orphaned on deletion. Now uses the correct locale config.

- **Deferred image conversions not cancelled on document delete** —
  When an upload document was deleted, pending image queue entries
  (deferred AVIF/WebP conversions) were not cleaned up. The scheduler
  would attempt to process them, fail because the source was deleted,
  and waste retries. Now cancels pending entries in all delete paths:
  single delete, bulk DeleteMany, Lua delete/delete_many, empty trash,
  and auto-purge.

- **Bulk DeleteMany missing upload file cleanup for image queue** —
  The gRPC `DeleteMany` and Lua `delete_many` did not cancel pending
  image queue entries for deleted documents. Now cleans up alongside
  the existing upload file deletion.

- **Debug logs shown in production** — The default stdout log filter
  for `serve` was `crap_cms=debug,info`, flooding production logs with
  debug output. Now defaults to `crap_cms=info` for production and
  `crap_cms=debug,info` only when `dev_mode = true`. File logging
  retains debug level for diagnostics. Override with `RUST_LOG` env
  var when needed.

- **SHA256 checksums missing from releases** — The release workflow
  now generates a `SHA256SUMS` file and uploads it alongside the
  binaries, enabling the install script to verify downloads
  automatically.

- **gRPC write handlers acquired two pool connections** — The gRPC
  Create, Update, and Delete handlers acquired a connection for
  auth/access checks, dropped it, then the service layer acquired a
  second one. Under high concurrency this caused pool exhaustion and
  5+ second latencies. Now reuses the same connection via
  `_with_conn` service variants, halving pool pressure on writes.

- **SQLite performance defaults too conservative** — Added configurable
  `cache_size` (default 16MB), `mmap_size` (default 256MB),
  `temp_store = MEMORY`, and `wal_autocheckpoint` to `[database]`
  config. Previous defaults used SQLite's 2MB cache with no
  memory-mapping. Pool `max_size` default raised from 32 to 64.

- **Lua VM pool capped at 32 regardless of hardware** — The auto-sized
  `vm_pool_size` default was clamped to `max(cpus, 4)` with a ceiling
  of 32, limiting concurrent hook execution on powerful servers.
  Removed both the floor and ceiling — now auto-sizes to exactly the
  number of CPU cores (fallback: 4 if detection fails). Override with
  `hooks.vm_pool_size` in `crap.toml`.

- **SQLite statements re-parsed on every query** — All `query_all`
  and `query_one` calls used `prepare()` which parses SQL from scratch
  on every invocation. Switched to `prepare_cached()` which reuses
  previously parsed statements. Reduces CPU overhead on every database
  operation, especially for hot paths like `find` and `find_by_id`.

- **Trash pagination navigated away from trash view** — The "Next" and
  "Previous" buttons in the trash list did not preserve `?trash=1` in
  the pagination URLs. Clicking next navigated to the regular (non-
  trash) document list. Now preserves the trash parameter across all
  pagination links.

- **Load test cleanup failed on soft-delete collections** — The gRPC
  load test script's cleanup used `DeleteMany` without
  `forceHardDelete`, so soft-delete collections reported 0 deleted.
  Now uses `forceHardDelete: true` and sums both `deleted` and
  `softDeleted` counts.

- **Example cache not enabled** — The example project now
  enables the relationship populate cache with a 60-second TTL,
  gRPC compression, and optimized depth/polling settings for better
  out-of-the-box demo performance.

- **Unpublish returned stale `_status`** — The unpublish operation
  (both collection and global) returned the document with
  `_status: "published"` instead of `"draft"` in the response. The
  database was updated correctly, but the in-memory document handed to
  after-hooks and returned to the caller still carried the pre-unpublish
  status. Now correctly sets `_status = "draft"` before after-hooks run.

- **Redundant junction table delete** — `set_polymorphic_related()`
  executed the same DELETE twice in the non-locale branch: once via the
  `delete_junction_rows()` helper and once via a manual inline query.
  Removed the redundant manual delete.

- **Flaky subscribe test race condition** — `subscribe_receives_update_event`
  and `subscribe_receives_delete_event` could receive a stale "create" event
  instead of the expected operation because event publishing runs in a
  background `spawn_blocking` task. Added a small delay between document
  creation and subscribing so the background create-event publish completes
  before the subscriber is registered.

- **Keyboard focus indicators suppressed** — `outline: 0` /
  `outline: none` on focused form inputs and the search bar removed
  the keyboard focus indicator, making the admin UI inaccessible for
  keyboard-only users. Added `focus-visible` outline styles so keyboard
  users see a visible focus ring while mouse users still get the clean
  appearance.

## [0.1.0-alpha.3] — 2026-03-30

### Added

- **Soft deletes** — Collections can opt into soft deletes with
  `soft_delete = true`. Deleted documents are moved to trash (`_deleted_at`
  timestamp) instead of being permanently removed. Soft-deleted documents
  are excluded from all reads, counts, and search. The admin UI shows a
  **Trash** tab with restore and permanent-delete buttons, plus an
  **Empty trash** action. Upload files are preserved until hard purge.
  Configurable retention (`soft_delete_retention = "30d"`) auto-purges
  expired documents. Granular permissions: `access.trash` controls
  soft-delete and undelete (falls back to `access.update`), while
  `access.delete` controls permanent deletion. Available in admin UI,
  gRPC (`Delete` with `force_hard_delete`, new `Undelete` RPC), and Lua
  (`crap.collections.delete/undelete` with `forceHardDelete` option).

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

- **Serve lifecycle management** — `crap-cms serve --stop` gracefully stops a
  detached instance (SIGTERM with 10s timeout, then SIGKILL). `--restart` stops
  and re-launches. `--status` shows whether a detached instance is running, with
  PID and uptime on Linux. Stale PID files are automatically cleaned up.

- **File-based logging** — optional `[logging]` config section writes logs to
  rotating files in `data/logs/`. Supports daily, hourly, or no rotation with
  configurable retention (`max_files`). Old log files are pruned on startup.
  Auto-enabled when running with `--detach` (where stdout is unavailable).
  New CLI command: `crap-cms logs` to tail log output (`-f` to follow,
  `-n` for line count), `crap-cms logs clear` to remove old rotated files.

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

- **`DeleteMany` soft-delete support** — `DeleteManyRequest` gains a
  `force_hard_delete` field (matching single `Delete`). When the collection
  has `soft_delete` enabled, `DeleteMany` now moves documents to trash by
  default. `DeleteManyResponse` reports both `deleted` (permanently removed)
  and `soft_deleted` (trashed) counts. Permission checks use `access.trash`
  for soft deletes and `access.delete` for hard deletes.

- **Bulk operation safety limit** — `UpdateMany` and `DeleteMany` are now
  capped at 10,000 documents per request to prevent unbounded memory usage.
  Use paginated calls for larger datasets.

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

- **Design system harmonization** — Unified button, input, and icon sizing
  across the entire admin UI. All interactive controls now share a consistent
  height scale derived from a single `--base` unit (4px grid). Buttons and
  inputs align at 36px (`--control-lg`), small buttons at 28px (`--control-sm`).
  Icon sizes use a dedicated `--icon-xs/sm/md/lg/xl` scale. All spacing,
  sizing, and layout values use `rem` units via `calc(var(--base) * n)` for
  scalability. The `button--secondary` variant (tinted) fills a previously
  missing gap between primary and ghost buttons.

- **Inline create for relationship fields** — Clicking "Create new" on a
  relationship or upload field now opens a near-fullpage slideout panel
  instead of navigating away. The create form loads in the panel with full
  field support (richtext, code, arrays, blocks). On success, the created
  item is automatically selected in the relationship field. Form context
  is preserved — no more losing unsaved work. Works for both has-one and
  has-many relationships, including polymorphic and upload fields.
  Ctrl+click still opens in a new tab for progressive enhancement.

- **Tag-style chips for has-many relationships** — Has-many relationship
  fields now display selected items as chips inside the search input
  (like a tag input), instead of in a separate row above. Backspace
  removes the last chip. Enter selects the first search result without
  requiring arrow-key navigation first.

- **Shadow DOM web components** — `<crap-block-picker>`, `<crap-tags>`,
  and `<crap-focal-point>` migrated to Shadow DOM with encapsulated
  styles. `<crap-relationship-search>` and `<crap-live-events>` use
  injected scoped styles. ~500 lines of CSS removed from global
  stylesheets and co-located with their components. Dead CSS for
  filter-builder and column-picker (duplicated in the drawer's Shadow
  DOM) removed from global sheets.

- **FOUC prevention** — `:not(:defined)` CSS rules hide Shadow DOM
  components until their JavaScript registers, preventing flash of
  unstyled content.

- **Event-driven component communication** — Removed all global
  singleton patterns (`window.CrapToast`, `getDrawer()`,
  `getConfirmDialog()`, `getCreatePanel()`). Components now
  communicate exclusively via native `CustomEvent` dispatch and
  document-level listeners. Zero cross-component imports, zero
  wrapper functions, zero null checks. Events used:
  `crap:toast` (notifications), `crap:drawer-request` (drawer
  discovery), `crap:confirm-dialog-request` (confirm dialog
  discovery), `crap:create-panel-request` (create panel discovery).

### Fixed

- **Unique check swallowed database errors** — When the uniqueness query
  failed (e.g. database connectivity issue), the error was logged at `warn`
  level but validation silently passed. Duplicate values could be persisted
  if the database was temporarily unavailable during validation. Now produces
  a `validation.unique_check_failed` error.

- **Custom validator errors silently passed** — When a Lua `validate`
  function threw a runtime error, the exception was logged at `warn` level
  but the field silently passed validation. Invalid data could be persisted.
  Now produces a `validation.custom_error` error.

- **`delete_many` silently skipped referenced documents** — When a bulk
  hard delete encountered documents with outstanding references, they were
  silently skipped with only a debug log. The caller received only the
  `deleted` count with no indication that some documents were not removed.
  Both Lua and gRPC `delete_many` now report a `skipped` count alongside
  `deleted`.

- **Has-many validation only reported first invalid value** — Per-element
  validation of has_many text/number fields used `break` after the first
  error, hiding subsequent violations. Users had to fix one value, resubmit,
  and discover the next. Now all invalid values are reported at once.

- **Unique check silently skipped on invalid locale** — When locale
  sanitization failed for a localized unique field, the unique constraint
  check was silently skipped with only a debug-level log. Duplicate values
  could slip through. Now emits a validation error instead.

- **Display conditions failed open on error** — When a display condition
  Lua function threw an error or returned an unexpected type, the field was
  shown as a "safe default". This could expose access-controlled content.
  Now fails closed (hides the field) on error.

- **Upload Bearer token silently fell back to anonymous** — The HTTP upload
  endpoint treated `Authorization: Basic ...` (non-Bearer scheme) as
  anonymous access instead of returning 401. Misconfigured clients were
  silently unauthenticated.

- **Bulk operations silently capped at 10K documents** — `UpdateMany` and
  `DeleteMany` applied a `LIMIT 10000` to the query but did not inform
  the client when results were truncated. Partial mutations occurred with
  no feedback. Now returns `RESOURCE_EXHAUSTED` when the limit is hit.

- **`get_ref_count` returned 0 for missing documents** — The function
  could not distinguish "document has zero references" from "document
  does not exist", which could mask lookup failures in delete protection.
  Now returns `Option<i64>` (`None` for missing documents).

- **Backfill migration silently skipped errors** — `backfill_ref_counts`
  caught query errors at `debug` level and returned `Ok(())`, hiding
  corrupted junction tables. Ref counts could remain incorrect while the
  migration appeared to succeed. Errors are now logged at `warn` level.

- **Display condition evaluated empty field name** — A condition object
  with a missing or empty `"field"` key silently matched against an
  empty-string lookup instead of warning. Now logs a warning and defaults
  to showing the field.

- **Length validation counted bytes instead of characters** — `min_length`
  and `max_length` field validation used `s.len()` (byte count) instead of
  `s.chars().count()` (character count). Multibyte UTF-8 strings were
  overcounted: "café" (4 chars, 5 bytes) would fail `max_length = 4`, and
  CJK text like "你好世界" (4 chars, 12 bytes) would fail `min_length = 5`.

- **Email validation accepted invalid dot patterns** — `is_valid_email_format`
  accepted leading dots (`.user@example.com`), trailing dots
  (`user.@example.com`), consecutive dots (`user..name@example.com`), and
  the same patterns in domain parts. Now rejects all per RFC 5321.

- **Empty Bearer token treated as valid** — `extract_bearer_token("Bearer ")`
  returned `Some("")` instead of `None`, which would pass to JWT validation
  and produce a confusing error. Now filters empty tokens.

- **FTS sync dropped existing index on validation failure** — `sync_fts_table`
  dropped the existing FTS table before validating field names. If validation
  failed (e.g., invalid identifier), the existing working index was destroyed
  with no replacement. Validation now runs before the drop.

- **Lua `delete` silently succeeded on missing documents** — The Lua CRUD
  `crap.collections.delete()` did not check the return value of
  `query::delete`, so deleting a non-existent document appeared to succeed.
  Now returns a "not found" error. `delete_many` now skips already-deleted
  documents gracefully instead of failing.

- **Delete error response leaked internal details** — The admin delete
  handler returned `e.to_string()` in JSON error responses, potentially
  exposing database paths, schema details, or internal error messages.
  Now returns a generic "Failed to delete item" message and logs the full
  error server-side.

- **Stale job error message showed wrong timeout value** — The
  `recover_stale_jobs` error message logged the stale detection threshold
  (2x timeout, min 300s) as `timeout=<threshold>s` instead of the actual
  configured job timeout, misleading operators.

- **Empty trash could delete referenced documents** — The "Empty trash"
  action permanently deleted all soft-deleted documents without checking
  `_ref_count`, which could break referential integrity. Now skips
  documents that are still referenced by other documents, matching the
  behavior of single delete and the gRPC `DeleteMany` endpoint.

- **Lua `delete_many` blocked soft-delete of referenced documents** —
  `crap.collections.delete_many` checked `_ref_count` for both soft and
  hard deletes, blocking soft-deletion of referenced documents. This was
  inconsistent with single `delete()` and the gRPC API, which only check
  ref counts for hard deletes. Soft-deleted documents remain referenceable
  by design.

- **Lua `delete_many` missing `forceHardDelete` option** —
  `crap.collections.delete_many` now supports `{ forceHardDelete = true }`
  to permanently delete documents even when the collection has
  `soft_delete` enabled, matching the existing single `delete()` API.

- **Table rebuild could leave database inconsistent on failure** — The
  SQLite table rebuild (used during `soft_delete` migration) could leave
  the database with an empty new table and orphaned temp table if the
  data copy step failed. Now recovers by restoring the original table.

- **Draft versioned updates skipped field-level after_change hooks** —
  When saving a draft version via Lua `crap.collections.update`, field-
  level `after_change` hooks were not called, though collection-level
  hooks were. Now both run consistently.

- **CSRF token extraction in list-settings.js** — The column settings
  save used `split('=')[1]` to extract the CSRF cookie, which would
  truncate tokens containing `=`. Now uses the same regex pattern as
  all other components.

- **API upload DELETE returned 500 for all errors** — The upload DELETE
  endpoint now returns `404 Not Found` when the document doesn't exist
  and `409 Conflict` when the document is referenced by others, instead
  of `500 Internal Server Error` for every failure.

- **Display condition errors silently showed fields** — When a Lua
  display condition function throws an error or returns an unexpected
  type, the field was shown without any diagnostic. Now logs a warning
  with the function reference and error details.

- **Access constraint unexpected types silently denied** — When an access
  function returns an unexpected Lua type (not boolean or table), the
  request was silently denied without logging. Now logs a warning with
  the function reference and actual type returned.

- **Transaction commit errors silently continued** — Three instances in
  the gRPC field-read-access path logged commit failures with
  `tracing::warn!` but continued execution. Now propagates the error
  properly via `?`.

- **Redundant timezone variable in create/update** — `tz_base` was
  identical to `tz_key` in both `create.rs` and `update.rs` timezone
  companion column handling. Removed the duplicate.

- **`<crap-create-panel>` never instantiated** — The `<crap-create-panel>`
  Web Component was imported and defined but never placed in the DOM,
  making the inline-create feature for relationship and upload fields
  completely non-functional. Added to `templates/layout/base.hbs`.

- **gRPC `get_global_impl` double pool acquisition** — Acquired a
  connection from the pool and then called `ops::get_global()` which
  acquired a second one, risking deadlock on small pools. Now uses
  `query::get_global()` directly on the existing connection.

- **gRPC `update_global_impl` held connection during service call** —
  Held a pool connection while `service::update_global_document()` tried
  to acquire its own, risking deadlock. Now drops the connection first.

- **Lua `update_many` accepted password on auth collections** — The Lua
  `crap.collections.update_many()` did not reject or strip password
  fields on auth collections. Bulk password changes are now explicitly
  rejected with a clear error message.

- **gRPC `restore_version_impl` leaked read-denied fields** — The
  restore-version endpoint returned the full document without stripping
  fields the user is not permitted to read. Now applies the same
  `strip_denied_proto_fields` as all other endpoints.

- **Global unpublish bypassed lifecycle hooks** — Unpublishing a global
  via the admin UI directly called `unpublish_with_snapshot` without
  running before/after change hooks. Now uses a new
  `unpublish_global_document()` that follows the same lifecycle as
  collection unpublish.

- **Lua `update_many` validation missing `soft_delete`, `registry`,
  `draft`** — The `ValidationCtx` in `update_many` was missing
  `soft_delete` (causing false-positive unique constraint violations on
  soft-delete collections), `registry` (skipping richtext node attribute
  validation), and `draft` (enforcing required-field checks on drafts).

- **`locale_config` not passed to `persist_create`/`persist_update`** —
  Reference count operations during create and update used a default
  (empty) `LocaleConfig`, potentially missing locale-specific relationship
  fields. Now forwards the locale config from the write context.

- **Verification email URL hardcoded `http://`** — The email verification
  URL always used `http://` regardless of configuration. Now respects
  `public_url` from server config, matching the forgot-password flow.

- **gRPC `get_global_impl` passed `user: None` to `AfterReadCtx`** —
  After-read hooks saw no authenticated user, breaking user-dependent
  transformations. Now passes the resolved auth user.

- **`send_signal` cast u32 PID to i32 via `as`** — PIDs above
  `i32::MAX` silently wrapped to negative values, which `kill(2)`
  interprets as process groups. Now uses `i32::try_from()` and returns
  an error for out-of-range PIDs.

- **MCP filter operators inconsistent with gRPC API** — MCP used
  `greater_than_equal`/`less_than_equal` while gRPC used
  `greater_than_or_equal`/`less_than_or_equal`. Both forms are now
  accepted. Unrecognized operators now log a warning instead of being
  silently dropped.

- **`me_impl` did not hydrate join table data** — The `/me` endpoint
  returned documents without hydrating array fields, has-many
  relationships, or blocks data. Now calls `hydrate_document`.

- **`list_job_runs_impl` had no upper bound on `limit`** — A client
  could pass an arbitrarily large limit. Now capped at 1000.

- **`empty_trash_action` called `fts_delete` unconditionally** — Did not
  check `supports_fts()` first, which would fail on non-FTS backends.

- **`delete_upload_files` skipped all `*image*` field names** — The
  filter `key.contains("image")` incorrectly skipped fields like
  `hero_image_url`. Changed to exact match on `image_url` only.

- **`ValidationError::to_field_map()` dropped duplicate field errors** —
  Multiple validation errors for the same field were lost due to
  `HashMap::collect()`. Now joins them with `"; "`.

- **Richtext custom node attribute roundtrip** — HTML-escaped attribute
  values (`&#39;`, `&amp;`, etc.) in `<crap-node data-attrs>` were not
  unescaped before JSON parsing, causing deserialization failures.

- **MIME verification bidirectional match** — The upload MIME check
  tested both directions (`detected ∈ claimed` OR `claimed ∈ detected`),
  weakening the security check. Now only verifies `detected ∈ claimed`.

- **`_tz` companion columns not locale-expanded** — When a localized
  Date field had `timezone = true`, `get_expected_column_names` generated
  bare `field_tz` instead of per-locale `field_tz__en`, `field_tz__de`.
  This caused migration drift detection to incorrectly flag columns.

- **Unquoted table names in trash/scheduler SQL** — `find_purge_candidates`
  and `purge_soft_deleted` used unquoted table names, which would fail
  for collection slugs that are SQL reserved words.

- **UTF-8 panic in config duration/filesize parsing** — Multi-byte
  characters (e.g., emoji) in `parse_duration_string` or
  `parse_filesize_string` could cause a panic from invalid byte-offset
  slicing. Now uses char-aware splitting and ASCII validation.

- **Inconsistent duration parsing in scheduler** —
  `parse_retention_seconds` only supported `d`/`h` suffixes. Now also
  supports `m` (minutes) and `s` (seconds) for consistency with
  `parse_duration_string`.

- **`before_broadcast` hooks lost `context` table** — The
  `call_before_broadcast_hook` and `call_registered_before_broadcast`
  functions did not call `read_context_back()`, silently discarding any
  shared state set by hooks on `ctx.context`.

- **`password.hbs` double-nested `form__field` wrapper** — The password
  field template included its own `<div class="form__field">` while the
  parent (`edit_form.hbs`) already provides one, causing CSS layout issues.

- **`_collectFormData` overwrote multi-value form fields** — Both
  `conditions.js` and `validate-form.js` used `data[key] = val` which
  dropped all but the last value for multi-value fields (has-many). Now
  collects duplicate keys into arrays.

- **Lua typegen sub-type name collisions** — Array/Group sub-type class
  names in Lua type generation used only the field name (e.g.,
  `crap.array_row.Items`), colliding when multiple collections had
  identically named fields. Now prefixed with the collection name.

- **EventBus used `Ordering::Relaxed` for sequence counter** — Could
  cause out-of-order sequence numbers across threads. Changed to
  `Ordering::AcqRel`.

- **`back_references` endpoint had no access control** — The endpoint
  returned back-references for any document without checking collection
  read access. Now verifies read permissions.

- **Session guard dialog accumulated event listeners** — The `show()`
  method added click/cancel listeners without removing previous ones.
  Now cleans up the `cancel` handler alongside click handlers.

- **Version list pagination generated `page=0` URLs** — Previous-page
  URLs for version lists used `page - 1` which produced `?page=0` on
  the first page. Now clamps to a minimum of 1.

- **Back-reference self-ref filter compared slug to ID** — The
  self-reference filter compared `owner_slug` (collection name) with
  `target_id` (document ID), making it effectively a no-op. Now
  correctly compares `owner_slug` with `target_collection`.

- **`jobs show` always printed Data field** — Used `if let Some(ref data)
  = Some(...)` which is always true. Changed to `if !run.data.is_empty()`.

- **Claims builder `iat` cast could wrap on pre-epoch clock** — Cast
  `i64` timestamp to `u64` via `as` which wraps negative values. Now
  clamps to 0 first.

- **Relationship search drawer race condition** — The drawer picker
  for relationship fields had no `AbortController`, so rapid searches
  or pagination could resolve out of order. Added abort controller to
  cancel stale fetches.

- **validate-form.js memory leak on reconnect** — Missing `_connected`
  guard meant event listeners could be duplicated if the component was
  disconnected and reconnected by HTMX swaps.

- **sessionStorage errors in private browsing** — `scroll.js` form
  state save/restore now wraps `sessionStorage` calls in try-catch to
  handle private browsing and quota exceeded scenarios gracefully.

- **Back-references button stuck on error text** — After a fetch error
  the "Show details" button displayed "error" permanently. Now restores
  the original button label on retry.

- **Invalid SQL in reference counting** — `MAX(0, expr)` is not
  portable across database backends. Replaced with
  `conn.greatest_expr()` on the `DbConnection` trait (SQLite uses
  `MAX(a, b)`, PostgreSQL would use `GREATEST(a, b)`).

- **Panic in date normalization** — `unwrap()` on
  `date.and_hms_opt()` replaced with proper error propagation via
  `ok_or_else()`.

- **Silent transaction commit errors** — 22 instances of
  `let _ = tx.commit()` across the codebase now log failures via
  `tracing::warn!` instead of silently swallowing errors.

- **Button/input disabled states** — `.button:disabled` now shows
  50% opacity with `not-allowed` cursor. Disabled inputs, selects,
  and textareas show dimmed text, grayed background, and block
  pointer events.

- **Sort on fields inside layout wrappers** — Sorting by a field
  inside a Row, Collapsible, or Tabs wrapper (e.g. `default_sort =
  "-start_date"` where `start_date` is in a Row) caused a 500 error
  ("Invalid sort column"). The sort column validator now recurses into
  layout wrappers to find promoted fields.

- **Upload fields in new block rows not saving** — When adding a new
  block row and selecting an upload/relationship, the value was lost
  on save. The `__INDEX__` placeholder in the `field-name` attribute
  of `<crap-relationship-search>` was not replaced with the actual
  row index, so the hidden input submitted an unparseable field name.
  Fixed by including `[field-name]` in the index replacement
  selectors.

- **Reference counting missing in bulk operations** — `UpdateMany`
  never adjusted ref counts when relationship fields changed, and
  `DeleteMany` never decremented target ref counts before deleting.
  Both could silently corrupt `_ref_count` values, breaking delete
  protection and creating dangling references. Now both operations
  snapshot and adjust ref counts per-document. `DeleteMany` also
  skips documents with `_ref_count > 0` (matching single-delete
  behavior).

- **Version restore broke reference counts** — Restoring a version
  snapshot never adjusted ref counts. If a relationship changed
  between versions, restoring the old version would leave the new
  target's count too high and the old target's count too low. Now
  snapshots outgoing refs before restore and applies the diff after.

- **Empty trash used wrong locale config** — The empty trash handler
  used `LocaleConfig::default()` instead of the site's actual locale
  configuration. Ref count adjustments for multi-locale sites with
  localized relationship fields could read the wrong locale columns.

- **FTS search skipped fields inside layout wrappers** — Fields
  inside Row, Collapsible, or Tabs (which promote children to
  parent-level columns) were not found by the FTS field validator.
  `list_searchable_fields` referencing such fields were silently
  filtered out. Now recurses into layout wrappers for both explicit
  and default FTS field resolution.

- **Upload path traversal when directory missing** — The
  canonicalize-based path check in the upload file serve handler was
  inside an `if let` that silently skipped the check when either path
  couldn't be canonicalized (e.g., directory doesn't exist). Changed
  to `match` — canonicalize failures now return 404.

- **Startup validation for field references** — Collection
  registration now warns when `use_as_title`, `default_sort`, or
  `list_searchable_fields` reference field names that don't exist in
  the collection's field definitions (including fields inside layout
  wrappers). Previously these misconfigurations failed silently at
  runtime.

- **JWT validation errors now logged** — Failed JWT token validation
  (expired, invalid signature, malformed) is now logged at debug
  level instead of being silently swallowed via `.ok()`. Aids
  debugging session issues in production.

- **Array date fields missing timezone columns** — Date sub-fields
  with `timezone = true` inside Array fields did not get the `_tz`
  companion column in the join table (both CREATE and ALTER TABLE
  paths). Main collection tables handled this correctly; array tables
  were missing the logic. Timezone data for array date fields was
  silently lost.

- **Inherited localization missing in join tables** — Arrays, Blocks,
  and has-many Relationships inside a localized Group did not inherit
  the `_locale` column in their join tables. Only directly-localized
  fields got the column. The `sync_join_tables_inner` function now
  propagates `inherited_localized` from parent Groups, matching the
  existing behavior in `collect_column_specs_inner`.

- **Inconsistent SQL identifier quoting** — Table names in SQL format
  strings were inconsistently quoted across the query layer. Some files
  (e.g., `ref_count.rs`) used double-quoted identifiers while most
  others did not. All table name interpolations now use double-quoted
  identifiers (`"table"`) for defense-in-depth consistency.

- **Global tables missing timezone companion columns** — Date fields
  with `timezone = true` in globals did not get the `_tz` companion
  column (both CREATE and ALTER TABLE paths). The column was created
  with the field's own type instead of TEXT, or omitted entirely.
  Collection tables handled this correctly; global migration code was
  missing the `companion_text` check. Timezone data for global date
  fields was silently lost or stored with the wrong type.

- **Global tables missing default values** — Fields with
  `default_value` in globals never had their SQL DEFAULT clause
  applied (both CREATE and ALTER TABLE paths). Collection tables
  handled this correctly; global migration code never called
  `append_default_value`. Checkbox fields also missed their implicit
  `DEFAULT 0`. New rows inserted into global tables got NULL instead
  of the configured default.

- **gRPC RestoreVersion not wrapped in transaction** — The gRPC
  `RestoreVersion` handler performed multiple SQL operations (update
  document, adjust ref counts, set status, create version) on a bare
  connection without a transaction. A failure partway through could
  leave the document in an inconsistent state. The admin UI handler
  was already correctly wrapped. Now both paths use a transaction.

- **Lua CRUD validation missing registry and soft_delete** — The Lua
  API's `crap.collections.create()` and `crap.collections.update()`
  called field validation without the registry (needed for richtext
  custom node attribute validation) and without the `soft_delete` flag
  (needed for unique constraint checks to exclude soft-deleted
  documents). This meant unique fields on soft-delete collections
  could reject values that only exist in soft-deleted rows, and
  richtext custom node validation was silently skipped. Also fixed
  the missing `soft_delete` flag in the bulk API `UpdateMany` and
  admin validation handlers.

- **Path traversal in upload file deletion** (CRITICAL) — The
  canonicalize-based path safety check in `delete_upload_files()` was
  inside an `if let` guard that only triggered when both canonicalize
  calls succeeded AND the path was outside the uploads directory. When
  canonicalize failed (e.g., broken symlink, missing directory), the
  guard didn't fire and the file was deleted without validation. Changed
  to an explicit `match` that skips deletion when canonicalize fails.

- **Division by zero in image resize** (CRITICAL) — `resize_image()`
  divided by `img.height()` and `img.width()` without checking for zero,
  causing a panic on malformed images with zero dimensions. Now returns
  `None` for zero-dimension images, and callers skip the size with a
  warning.

- **Field hook modifications lost in after-change hooks** (CRITICAL) —
  Both `crap.collections.create()` and `crap.collections.update()` in
  the Lua API ran field-level `after_change` hooks that modified
  `after_data`, but then passed `doc.fields.clone()` (the unmodified
  data) to the collection-level `after_change` hook. Field hook
  modifications were silently discarded. Now passes `after_data` to
  the collection-level hook.

- **Unpublish after-change hook received stale data** (HIGH) — The
  `after_change` hook for unpublish operations received the pre-unpublish
  document data with `draft: false`. Now re-reads the document after
  the unpublish and passes the updated state with `draft: true`.

- **DeleteMany deleted upload files for ref-protected documents**
  (HIGH) — `DeleteMany` iterated all queried documents for file cleanup,
  including those skipped due to `_ref_count > 0`. Database records
  survived but their upload files were deleted. Now only deletes files
  for documents that were actually removed from the database.

- **DeleteMany fired BeforeDelete hook for skipped documents** (HIGH) —
  `DeleteMany` ran the `BeforeDelete` hook before checking reference
  counts. Documents with incoming references were skipped (not deleted),
  but the hook had already fired, causing semantic inconsistency. Moved
  the reference count check before the hook.

- **Soft-delete purge deleted files before database records** (HIGH) —
  `purge_collection()` deleted upload files before the corresponding
  database delete. A crash between the two operations left orphaned
  database records pointing to missing files. Reversed the order: DB
  delete first, then file cleanup. A crash now leaves orphaned files
  (harmless) instead of orphaned records (harmful).

- **Zero scheduler intervals caused busy loops** (HIGH) — `JobsConfig`
  allowed `poll_interval`, `cron_interval`, and `heartbeat_interval` to
  be set to 0, causing tokio interval timers to fire continuously and
  starve the event loop. Added startup validation that all three must
  be > 0.

- **DeleteMany ignored `soft_delete` configuration** (HIGH) — The gRPC
  `DeleteMany` always performed hard deletes, bypassing the collection's
  `soft_delete` setting entirely. Documents that should have been moved to
  trash were permanently destroyed. Now respects `soft_delete`: matching
  documents are soft-deleted unless `force_hard_delete` is set. Permission
  checks also now use `access.trash` for soft deletes (matching single
  `Delete` behavior) instead of always requiring `access.delete`.
  `DeleteManyResponse` now reports both `deleted` and `soft_deleted` counts.

- **Field access control skipped Tabs sub-fields** (HIGH) — Field-level
  access control (`access.read`, `access.create`, `access.update`) did
  not recurse into Tabs layout containers. Fields with access restrictions
  inside Tabs were silently exposed to all users. Now correctly recurses
  into `field.tabs[i].fields`. The `deny_all_access_controlled` fail-closed
  fallback (used when the Lua VM pool is exhausted) had the same issue and
  is also fixed to recurse into Group, Row, Collapsible, and Tabs.

- **Richtext/Code editors lost state on array row reorder** (HIGH) —
  `CrapRichtext` and `CrapCode` web components destroyed and re-initialized
  their editor views on every DOM disconnect/reconnect cycle (triggered by
  drag-and-drop reordering). Undo history, cursor position, and unsaved
  content could be lost. Added idempotency guards to `connectedCallback`
  and removed destructive cleanup from `disconnectedCallback`. Also fixed
  `CrapConditions` and `CrapBackRefs` registering duplicate event listeners
  on reconnection.

- **Unquoted SQL table names in migrations** — `CREATE TABLE`, `ALTER TABLE`,
  `DROP TABLE`, `INSERT INTO`, and `RENAME TO` statements in migration code
  did not double-quote table names. Collections with slugs matching SQL
  reserved words (e.g., `order`, `group`, `index`) would fail to create or
  alter. All migration SQL now uses `"table_name"` quoting.

- **Sort by group sub-fields rejected** — `is_valid_sort_column` did not
  recognize group sub-fields (`seo__title`) or fields inside Tabs. Sorting
  by these columns returned "Invalid sort column". Now handles `group__sub`
  naming and recurses into Tabs.

- **Cursor pagination broke with NULL sort values** — Keyset pagination
  used `col > ?` / `col < ?` comparisons which evaluate to NULL in SQL
  when the cursor's sort value is NULL. All remaining rows were silently
  skipped. Now uses `IS NULL` / `IS NOT NULL` conditions for NULL cursors.

- **`field_exists_recursive` skipped Tabs** — Registry startup warnings
  for `use_as_title`, `default_sort`, and `list_searchable_fields` did not
  recurse into Tabs containers, producing false-positive "field not found"
  warnings for valid configurations. Now recurses into `field.tabs`.

- **Empty trash ignored `default_deny` setting** — The empty trash handler
  hard-coded a 403 when no `access.delete` function was configured,
  regardless of the `default_deny` setting. Now uses the same
  `check_access_or_forbid` pattern as other access checks.

- **Validate endpoints leaked internal error details** — Non-validation
  errors from the create/update validate endpoints included full
  `anyhow::Error` strings (potentially containing DB paths, schema
  details) in the HTTP response. Now returns a generic message and logs
  the full error server-side.

- **Evaluate conditions accepted arbitrary Lua function refs** — The
  server-side display condition evaluation endpoint accepted any Lua
  function reference string without validation. Now validates that
  submitted function refs match `admin.condition` values defined in the
  collection's field definitions.

- **Bulk operations had no query limit** — `UpdateMany` and `DeleteMany`
  loaded all matching documents into memory with no safety cap. A broad
  filter on a large collection could cause OOM. Now capped at 10,000
  documents per bulk operation.

- **Draft mode skipped all validation on Array/Blocks sub-fields** —
  Saving as draft skipped not just `required` checks but all validation
  (email format, numeric bounds, option values, custom validators) for
  Array and Blocks sub-fields. Now only skips `required` in draft mode;
  all other constraints are enforced.

- **MCP auth collection schema missing `password` in required** — When
  an auth collection had no other required fields, the `password` field
  was silently omitted from the `required` array in the MCP tool schema.
  LLM clients could create users without passwords.

- **MCP stdio panic lost request ID** — If `handle_message` panicked
  inside `spawn_blocking`, the error response was sent with `id: None`.
  MCP clients could not correlate the error with their request. Now
  preserves the request ID before moving it into the blocking task.

- **CrapTags ignored readonly attribute** — The tag input component
  did not check `data-readonly`, allowing users to add and remove tags
  on locale-locked or readonly fields. Now hides the input and remove
  buttons when readonly.

- **XSS in focal-point component** — `CrapFocalPoint` interpolated the
  image `src` directly into an `innerHTML` template literal, allowing
  attribute injection via crafted `data-src` values. Now sets `src` via
  the DOM property.

- **Delete dialog double-click race condition** — Rapid double-clicking
  the delete button could send duplicate DELETE requests before the first
  response arrived. Added a `submitting` guard.

- **Dirty form guard used wrong HTMX event property** — `CrapDirtyForm`
  and `CrapLiveEvents` accessed `e.detail.verb` on `htmx:beforeRequest`
  events, but HTMX 1.9 provides `e.detail.requestConfig.verb`. The dirty
  flag could be incorrectly cleared on GET navigations. Now checks both
  properties for compatibility.

- **Job retry backoff skipped the 5-second tier** — `backoff_seconds`
  used `2^attempt` but `attempt` is 1-based after claim, so the first
  retry waited 10s instead of 5s. Fixed formula: `2^(attempt-1) * 5`.

- **MCP global read used fragile string matching** — `exec_read_global`
  detected "not found" errors by checking if the error message contained
  "not found" or "no rows". Unrelated errors containing those substrings
  would be silently swallowed. Now inspects the error chain for specific
  causes.

- **Cron expression normalization preserved extra whitespace** —
  `normalize_cron` prepended "0 " to the raw input string, so
  `"0  3  *  *  *"` became `"0 0  3  *  *  *"`. Now normalizes to
  single-spaced output.

- **i18n translations not refreshed on HTMX body swap** — The `t()`
  translation function cached the `#crap-i18n` data island on first
  access and never invalidated. After a locale change via HTMX navigation,
  stale translations persisted until a full page reload. Now invalidates
  the cache on `htmx:afterSettle` body swaps.

- **CSRF cookie decoding inconsistency** — `validate-form.js` and
  `conditions.js` read the CSRF cookie without `decodeURIComponent`,
  while `delete-dialog.js` decoded it. Now all components decode
  consistently.

- **Create panel error used innerHTML** — The error fallback in
  `CrapCreatePanel` used `innerHTML` with the `t('error')` translation
  string, which could render HTML if the translation contained markup.
  Now uses `textContent`.

- **Delete dialog error response double-consumed** — After a failed
  `resp.json()` parse, the catch block called `resp.text()` on the
  already-consumed body. Now reads the body once with `resp.text()` and
  parses via `JSON.parse`.

- **Image queue claim race condition** — `claim_pending_images()` used
  a non-atomic SELECT-then-UPDATE pattern. Concurrent callers could
  SELECT the same pending rows before either marked them as processing,
  leading to duplicate image processing. Now uses optimistic locking:
  each UPDATE includes `AND status = 'pending'` so only one caller
  succeeds per row.

- **Unknown block types silently bypassed validation** — Blocks fields
  with an unrecognized `_block_type` (not matching any defined block
  definition) were silently skipped during validation. Arbitrary data
  could be stored without any field validation. Now produces a
  `validation.unknown_block_type` error.

- **Non-object array/blocks rows silently bypassed validation** —
  Primitive values (strings, numbers, null) in array or blocks fields
  were silently skipped instead of being validated. Now produces a
  `validation.invalid_row_type` error when sub-fields or block
  definitions are defined on the field.

- **`has_many` select malformed JSON silently ignored** — A `has_many`
  select field with invalid JSON (e.g., `"[invalid"`) silently passed
  option validation. Now produces a
  `validation.invalid_multi_select_json` error.

- **Locale sanitization fell back to wrong column for unique check** —
  When a locale string failed `sanitize_locale()`, the unique constraint
  check fell back to the non-localized column name (e.g., `slug` instead
  of `slug__en`), potentially allowing duplicates in the localized
  column. Now skips the unique check entirely on invalid locale.

- **Default value type not validated against field type** — A field
  definition could have a type-mismatched `default_value` (e.g., boolean
  default on a text field, string default on a number field) without any
  error. Documents created without explicit values would get
  type-incompatible defaults. Now validates at parse time: checkbox
  requires boolean, number requires number, text/date/select/etc.
  require string.

- **`ClaimsBuilder.build()` panicked on missing fields** — The JWT
  claims builder used `.expect()` for required `email` and `exp` fields,
  which would panic and crash the server if a code path failed to set
  them. Now returns `Result` with descriptive error messages. All
  callers updated to handle the error gracefully.

- **JSON-to-Lua number conversion silently lost data** — JSON numbers
  outside the i64 and f64 representable range were silently converted
  to Lua `nil`, losing the value without any error. Now returns a
  `RuntimeError` describing the unrepresentable number.

- **CSRF cookie `decodeURIComponent` could throw** — The
  `_getCsrf()` helpers in `conditions.js`, `validate-form.js`, and
  `delete-dialog.js` called `decodeURIComponent()` without a try-catch.
  A malformed cookie value could throw an uncaught exception, breaking
  form submissions and condition evaluation. Now falls back to the raw
  cookie value on decode error.

- **Validation error elements missing `role="alert"`** — Error messages
  injected by `validate-form.js` did not have `role="alert"`, so screen
  readers would not announce validation errors to assistive technology
  users. Now sets `role="alert"` on all injected error elements.

- **Server-side condition evaluation race condition** — The
  `<crap-conditions>` component's debounced server-side evaluation had
  no request cancellation. Rapid form changes could result in multiple
  in-flight requests, with stale responses overwriting newer results.
  Now uses `AbortController` to cancel previous requests before
  issuing a new one.

- **Field-level hooks skipped nested fields** (CRITICAL) — `run_field_hooks_inner`
  and `has_field_hooks_for_event` only iterated top-level fields, never
  recursing into Group, Row, Collapsible, or Tabs containers. Field hooks
  (before_validate, before_change, after_change, after_read) defined on
  sub-fields inside these containers were silently skipped. Now uses
  recursive traversal with proper `group__subfield` prefix accumulation,
  matching the pattern already used by validation.

- **Unpublish before-change hook received `draft=false`** (HIGH) — Both the
  Lua CRUD `handle_unpublish` and the service-layer `unpublish_document`
  built the `beforeChange` hook context with `draft(false)` (or omitted it
  entirely), even though the document is transitioning to draft state. Hooks
  could not distinguish unpublish from a regular update. Now both paths set
  `draft(true)`.

- **`condition_is_truthy` treated `Number(0)` as truthy** — The display
  condition `is_truthy` / `is_falsy` operators treated all numbers
  (including zero) as truthy, inconsistent with standard truthiness
  semantics. `0` and `0.0` are now falsy. Both the Rust backend and
  JavaScript client-side evaluation are fixed.

- **Unknown display condition operators silently showed fields** — A
  condition object with an unrecognized operator (e.g., a typo like
  `"greater_than"` instead of `"equals"`) silently defaulted to showing
  the field. Now logs a warning with the field name.

- **Richtext link modal allowed `javascript:` URLs** — The link insertion
  dialog accepted any URL protocol, including `javascript:`, `data:`, and
  `vbscript:`. The server-side renderer already blocked these at output
  time, but the editor now also validates on input — only `http:`,
  `https:`, `mailto:`, `tel:`, and relative URLs are accepted.

- **Negative LIMIT/OFFSET passed to SQLite** — `FindQuery` accepted
  negative `limit` and `offset` values, which have undefined behavior in
  SQLite. Now clamped to 0 before binding.

- **gRPC auth silently downgraded deleted users to anonymous** — When a
  valid JWT referenced a user that was subsequently deleted, the gRPC
  `resolve_auth_user` returned `Ok(None)` instead of an error, silently
  treating the request as anonymous. Now returns `unauthenticated` error.

- **Bulk `UpdateMany`/`DeleteMany` bypassed per-document access checks** —
  When no access function was configured for a collection, bulk operations
  skipped per-document access checks entirely instead of delegating to the
  default access system. Now always runs access checks regardless of
  whether an explicit access function is configured.

- **Back-references used wrong junction table for Group-nested fields** —
  `back_references.rs` constructed junction table names without the group
  prefix (e.g., `posts_tags` instead of `posts_meta__tags` for a field
  inside a Group), causing delete protection to miss references through
  Group-nested has-many relationships, Arrays, and Blocks.

- **Locale write path ignored inherited Group localization** — When a Group
  had `localized: true`, its sub-fields got locale-suffixed columns in the
  database (via migrations), but the write path (`locale_write_column`)
  only checked each field's own `localized` flag. Data was written to the
  unsuffixed column but read from the locale-suffixed one, causing apparent
  data loss. Now propagates `inherited_localized` through write paths.

- **`_status` column missing from locale-mode queries** — Collections with
  both drafts and localization enabled did not include the `_status` column
  in locale-aware SELECT queries, while the non-locale path included it.
  Downstream code inspecting `_status` would find it absent. Added
  `get_locale_select_columns_full` which includes `_status` when
  `has_drafts` is true.

- **Upload file cleanup skipped on `force_hard_delete`** — When
  `force_hard_delete` was used on a soft-delete upload collection, the
  upload file cleanup was skipped because the condition only checked
  `!def.soft_delete`. Now also cleans up files when `force_hard_delete`
  is true.

- **Lua sandbox allowed native C module loading** — `package.cpath` and
  `package.loadlib` were not removed from the Lua sandbox. A hook author
  who could place a `.so`/`.dll` in the package search path could load
  arbitrary native code. Now clears `package.cpath`, removes
  `package.loadlib`, and removes `string.dump`.

- **`user delete` CLI command bypassed ref_count** — The CLI user delete
  command called `query::delete` directly, bypassing ref count decrements.
  This left stale `_ref_count > 0` values on referenced documents, making
  them undeletable. Now uses a transaction with `before_hard_delete`.

- **gRPC `Me` endpoint checked `_locked` via field value** — The `Me`
  endpoint inspected `doc.fields["_locked"]` instead of using the
  `query::is_locked()` DB query. If `_locked` was stripped by field-level
  access controls, the check would always pass. Now queries the DB
  directly, matching the login endpoint behavior.

- **gRPC `RestoreVersion` used deferred transaction** — `restore_version_impl`
  used `conn.transaction()` instead of `conn.transaction_immediate()`,
  which could cause SQLite `BUSY` errors under concurrent writes. Now
  uses immediate transaction like all other write operations.

- **`sqlite_date_offset_expr` double-negation on negative input** — The
  function always prepended `-` to the seconds value. If a negative value
  was passed (future offset), it would produce `--30 seconds` which SQLite
  cannot parse. Now uses absolute value with explicit sign.

- **Join table names not quoted in SQL** — Array, Block, and Relationship
  join table SQL statements used unquoted table names, which could cause
  subtle errors if table names contained SQL reserved words. Now
  consistently double-quotes all join table names.

- **Non-ASCII `X-Created-Label` header silently failed** — The inline
  create panel's `X-Created-Label` response header failed silently for
  non-ASCII document titles (e.g., accented characters, CJK) because HTTP
  headers only allow visible ASCII. Now percent-encodes the label, and the
  JS side decodes it.

- **Version list pagination accepted `per_page=0`** — The version list
  page (collections and globals) had no lower bound on `per_page`,
  allowing `per_page=0` which produced infinite empty pages. Now uses
  `.clamp(1, max_limit)`.

- **Email verification allowed for locked accounts** — The verify-email
  endpoint marked locked users as verified, inconsistent with the
  reset-password handler which rejects locked accounts. Now blocks
  verification for locked accounts.

- **CSRF token not URL-decoded in `<crap-create-panel>`** — The create
  panel extracted the CSRF cookie value without `decodeURIComponent()`,
  while other components (delete dialog, conditions) properly decoded it.
  Could cause CSRF validation failures. Now uses a shared decode pattern.

- **`<crap-dirty-form>` catch handler cleared dirty flag** — When the
  confirm dialog promise rejected, the `.catch()` handler silently cleared
  `this._dirty`, removing unsaved-changes protection. Now preserves the
  dirty flag on rejection.

- **`<crap-conditions>` stale form reference after HTMX swap** — The
  `_initialized` guard prevented re-initialization after disconnect/
  reconnect, leaving `_debouncedServer` bound to a stale form element.
  Now resets `_initialized` in `disconnectedCallback`.

- **`<crap-list-settings>` used `innerHTML` with translation strings** —
  The add-filter button concatenated `t('add_condition')` into `innerHTML`,
  which could be an XSS vector if translation strings were attacker-
  controlled. Now uses `createElement`/`textContent`.

- **`<crap-sidebar>` Escape handler fired when sidebar closed** — The
  Escape key handler closed the sidebar unconditionally even when already
  closed, potentially interfering with other Escape handlers (modals,
  dialogs). Now only fires when the sidebar is open.

- **Logout route comment said GET/POST** — The `logout_action` handler
  comment incorrectly documented `GET/POST` but the route only accepts
  POST (correct for CSRF protection). Fixed the comment.

- **Polymorphic junction table rebuild dropped foreign key** —
  `rebuild_junction_table_for_polymorphic` (upgrading a has-many
  relationship to polymorphic) recreated the table without the
  `REFERENCES parent(id) ON DELETE CASCADE` constraint on `parent_id`.
  Cascading deletes stopped working for upgraded junction tables, leaving
  orphaned rows when a parent document was deleted.

- **Session version lookup swallowed DB errors** — `resolve_auth_user`
  (API) and `load_auth_user` (admin) used `unwrap_or(0)` on the session
  version query. A transient database failure returned 0, which matched
  tokens with `session_version = 0` (never changed password), bypassing
  session invalidation after a password change. Now propagates the error
  and rejects the token on failure.

- **Ref-count backfill interpolated count into SQL** —
  `increment_ref_count` in `backfill_ref_counts.rs` embedded the count
  value directly in the SQL string via `format!` instead of using a
  parameterized placeholder. Now uses `conn.placeholder(2)`.

- **Scaffold accepted arbitrary field names** — `parse_field_token` did
  not validate field names, allowing special characters (quotes, spaces,
  semicolons) that could produce broken or injectable Lua output in
  generated collection files. Now rejects names that aren't alphanumeric
  plus underscore. Block type names and labels are now escaped for safe
  Lua string embedding.

- **`forceHardDelete` bypassed referential integrity** — The Lua CRUD
  `crap.collections.delete()` with `forceHardDelete = true` skipped the
  `_ref_count` check entirely, allowing hard-deletion of documents still
  referenced by others. This corrupted ref counts on target documents and
  created dangling references. Now always checks ref counts for hard
  deletes regardless of how they are triggered.

- **Array/Blocks sub-field validation incomplete** — Fields inside Array
  and Blocks rows only ran 4 of 9 validation checks (required, date format,
  custom Lua validate, richtext node attrs). Missing checks: `min_length`/
  `max_length`, `min`/`max` numeric bounds, email format, select option
  validation, and has-many element validation. A Text field with
  `max_length = 10` inside an Array accepted values of any length; a Number
  field with `min = 0` accepted negatives; a Select field accepted values
  not in the options list.

- **`has_many` length validation counted bytes** — Per-element `min_length`
  and `max_length` checks on has-many Text fields used `.len()` (byte
  count) instead of `.chars().count()`. Multibyte UTF-8 values (emoji, CJK,
  accented characters) were overcounted, producing false validation errors.

- **Filter validation rejected fields inside layout wrappers** — Array,
  Blocks, and has-many Relationship fields inside Row, Collapsible, or Tabs
  wrappers were not found by `get_valid_filter_paths`, causing API filter
  queries on those fields to be rejected with "Invalid field". The same
  issue existed in `resolve_filter` (SQL generation stage) which also did
  a flat lookup. Both now recurse into layout wrappers.

- **Version snapshot restore lost Group fields inside layout wrappers** —
  `extract_snapshot_data` did not recurse into Row/Collapsible/Tabs wrappers
  nested inside Groups. Restoring a version snapshot silently dropped those
  fields. Refactored to a recursive prefix-based approach matching the
  write path.

- **No server-side password requirement on auth user creation** — Creating
  a user in an auth collection via the admin UI with an empty password
  field succeeded silently, producing an account with no password hash
  that could never log in. The client-side `required` attribute was the
  only protection. Now returns a validation error server-side.

- **Password policy error rendered broken page** — When password policy
  validation failed during create or update, the handler rendered
  `collections/edit_form` with an empty JSON context (`&json!({})`),
  producing a blank page with only the toast error. Now returns a 422
  with only the toast header, so HTMX preserves the form content and the
  user sees the error without losing their input.

- **API `parse_where_json` rejected numeric and boolean shorthand** —
  Filter queries like `{"active": true}` or `{"count": 42}` were rejected
  with "value must be string or operator object". Clients had to use the
  verbose form `{"active": {"equals": "true"}}`. Now accepts numbers and
  booleans as shorthand equals filters, consistent with `value_to_string`
  which already supported them. Also fixed inside `or` groups.

- **`UpdateMany`/`DeleteMany` skipped draft filtering** — Bulk update and
  delete operations did not apply the draft status filter, potentially
  affecting draft documents that should have been excluded. `find` and
  `count` correctly applied this filter. `UpdateMany` now respects the
  `draft` request field; `DeleteMany` defaults to published-only.

- **`UpdateMany`/`DeleteMany` missing mutation events** — Bulk operations
  did not publish mutation events to the event bus, so `Subscribe` stream
  listeners were never notified of bulk changes. Now publishes per-document
  events after commit.

- **CSRF token not URL-decoded in list-settings.js** — The column picker
  save handler did not `decodeURIComponent()` the CSRF cookie value,
  unlike every other CSRF reader in the codebase. Tokens containing
  URL-encoded characters would fail with a CSRF mismatch.

- **Richtext link `rel` attribute lossy on edit** — Editing a link with
  `rel="nofollow noopener noreferrer"` showed the nofollow checkbox as
  unchecked (strict `=== 'nofollow'` comparison), and re-saving stripped
  all other rel tokens. Now uses `.includes('nofollow')` and preserves
  existing tokens.

- **Relationship view link bypassed SPA navigation** — Dynamically setting
  `hx-get` on the relationship field "view" link did not call
  `htmx.process()`, so HTMX never registered the attribute. Clicking the
  link caused a full page reload instead of SPA-style navigation.

- **Slow resize test allocated 1 billion pixels** — The
  `resize_image_cover_extreme_aspect_ratio_no_overflow` test used 1000x1 →
  1x1000 dimensions, causing a 1000000x1000 intermediate allocation that
  took over 60 seconds. Reduced to 10x1 → 1x10 which exercises the same
  ratio math instantly.

- **JSON template helper missing single-quote escape** — The `{{{json ...}}}`
  Handlebars helper only escaped `</` (for `<script>` breakout prevention)
  but not single quotes. When used in single-quoted HTML attributes like
  `data-condition='{{{json condition_json}}}'`, a value containing `'`
  could break out of the attribute. Now escapes `'` to `\u0027` (valid
  JSON unicode escape, decoded transparently by `JSON.parse`). Affects
  display condition attributes on fields, collapsibles, rows, tabs, groups,
  and sidebar sections.

- **Soft-delete purge skipped ref count checks** (CRITICAL) — The
  scheduler's `purge_collection()` hard-deleted expired soft-deleted
  documents without checking `_ref_count`, without calling
  `before_hard_delete()` to decrement outgoing references, and without
  cleaning up FTS entries. This silently broke referential integrity:
  referenced documents were permanently deleted, and target documents
  retained phantom ref counts that blocked their deletion. Now mirrors
  the `empty_trash` logic: checks ref count (skips referenced docs),
  decrements outgoing refs, cleans FTS, then deletes.

- **`delete_document` blocked soft-delete of referenced documents**
  (HIGH) — The single-document `delete_document` service function
  unconditionally checked `_ref_count > 0` before any delete, including
  soft deletes. Every other code path (gRPC `DeleteMany`, Lua `delete`,
  Lua `delete_many`, admin `empty_trash`) correctly only checks ref
  count for hard deletes. Users could not soft-delete (trash) documents
  that were referenced by other documents. Now only checks ref count
  for hard deletes.

- **gRPC field-level write access bypass via `join_data`** (HIGH) —
  The gRPC `Create`, `Update`, and `UpdateGlobal` endpoints stripped
  denied fields from the `data` map but not from `join_data`. Array,
  Blocks, and has-many relationship data for access-controlled fields
  could still be written through the gRPC API. The bulk `UpdateMany`
  endpoint correctly stripped both maps. Now all endpoints strip
  `join_data` as well.

- **Sub-field custom validator errors silently passed** (HIGH) — When a
  Lua `validate` function threw a runtime error inside an Array or
  Blocks sub-field, the error was logged but validation silently passed.
  The top-level `check_custom_validate` correctly failed validation on
  error; the sub-field counterpart did not. Invalid data could be
  persisted through Array/Blocks fields. Now produces a
  `validation.custom_error` error, matching the top-level behavior.

- **`restore_global_version` skipped ref count adjustment** (HIGH) —
  Restoring a version snapshot on a Global never adjusted ref counts.
  If a relationship field changed between versions, restoring the old
  version left the new target's count too high and the old target's
  count too low. The collection `restore_version` handled this
  correctly. Now snapshots outgoing refs before restore and applies
  the diff after.

- **AfterChange hooks missing document `id` in `ctx.data`** — The
  AfterChange hook context for single `create` and `update` operations
  in the Lua CRUD API received `doc.fields` without the document `id`.
  Hooks needing to reference the document (e.g., for follow-up
  operations or notifications) had no way to get it. The bulk
  `update_many` path correctly included `id`. Now all paths include it.

- **Delete confirmation page used wrong access check for soft-delete** —
  The admin delete confirmation page always checked `access.delete`,
  even for collections with `soft_delete` enabled. Users with
  `access.trash` permission (but not `access.delete`) were blocked from
  viewing the confirmation dialog, even though the soft-delete action
  itself would succeed. Now uses `resolve_trash()` for soft-delete
  collections.

- **Upload update loaded old document without locale context** — The
  HTTP upload PATCH endpoint loaded the old document for file cleanup
  with `locale_ctx = None`, while the admin handler equivalent correctly
  passed the locale context. On localized upload collections, this
  could return wrong field values, causing incorrect file cleanup
  (orphaned files or premature deletion).

- **gRPC `FindByID` documentation contradicted behavior** — The
  RPC-level comment stated that `FindByID` returns an empty document
  field when no match is found and that `NOT_FOUND` is not returned.
  The actual implementation returns a `NOT_FOUND` status error, as
  correctly documented in the `FindByIDResponse` message comment.
  Fixed the RPC-level comment to match actual behavior.

- **Undelete action silently redirected on failure** — The admin undelete
  action logged errors but always redirected to the trash page,
  regardless of whether the undelete succeeded. Users had no indication
  of failure. Now returns an HTTP 500 error response on failure.

- **Proto `FieldInfo.type` listed nonexistent field types** — The
  proto comment listed `multiselect`, `point`, and `color` as valid
  field types, none of which exist. Updated to match the actual
  `VALID_FIELD_TYPES` list.

- **Proto `scheduled_by` documentation mismatch** — The
  `GetJobRunResponse.scheduled_by` comment listed `"scheduler"` but
  the code sends `"cron"`. Fixed the comment.

- **Cron schedule test non-deterministic** — `check_cron_schedules_skips_not_due`
  used `chrono::Utc::now()` with a 1-second window, which could
  non-deterministically fire if run at an exact hour boundary. Now uses
  a fixed time at minute :30 to guarantee deterministic behavior.

- **Lua RuntimeError lost anyhow cause chain** — All `RuntimeError`
  conversions in the hooks system used `format!("{}", e)` which only
  printed the top-level error message. Nested causes from `anyhow`
  errors (e.g., SMTP connection errors, DB errors) were silently
  discarded, making job failures and hook errors difficult to diagnose.
  Now uses `format!("{:#}", e)` to print the full cause chain.

- **`jobs list` CLI did not show errors** — The `jobs list` table only
  showed ID, Job, Status, Attempt, and Created. Failed jobs required
  `jobs show <id>` to see the error. Now includes a truncated Error
  column in the list view for at-a-glance diagnosis.

- **Example Lua files missing `overrideAccess`** — The example seed
  migration, jobs (`process_inquiry`, `cleanup_archived`,
  `weekly_report`), hooks (`prevent_last_admin`), and access strategy
  (`api_key_strategy`) did not pass `overrideAccess = true` to CRUD
  calls. After the `overrideAccess` default change to `false`, these
  all failed with "access denied" at runtime. All example Lua files
  now explicitly set `overrideAccess = true`.

- **AfterChange hooks missing `id` in draft-version update** (HIGH) —
  When saving a draft version via Lua `crap.collections.update`, the
  AfterChange hook context did not include the document `id` in
  `ctx.data`. Now includes `id` before running field-level and
  collection-level hooks.

- **AfterChange hooks missing `id` in unpublish** (HIGH) — The
  unpublish code path built the AfterChange context from
  `updated_doc.fields` without inserting the document `id`. Hooks
  could not identify which document was unpublished.

- **`update_many` AfterChange field hooks missing `id`** — The
  `id` was inserted into `after_data` after field-level AfterChange
  hooks had already run, so field hooks saw no `id` while
  collection-level hooks did. Moved the insertion before field hooks
  for consistency with single create/update.

- **Admin login leaked account state** (HIGH) — When a correct
  password was provided for a locked or unverified account, the admin
  login returned distinct error messages (`error_account_locked`,
  `error_verify_email`), confirming password correctness and account
  state to an attacker. The gRPC handler already returned a generic
  error for all cases. Now both return the same generic "invalid
  credentials" response, with the actual reason logged at debug level.

- **XSS in example richtext CTA/mention renders** (HIGH) — The
  example `init.lua` CTA and mention custom richtext node render
  functions interpolated user-controlled attributes (`url`, `text`,
  `style`, `name`) directly into HTML via `string.format` without
  escaping. A CMS author could inject arbitrary HTML/JS into the
  public-facing site. Now escapes all attributes with an
  `html_escape` helper.

- **Upload API delete used wrong access check for soft-delete** —
  The upload DELETE endpoint always checked `access.delete` even when
  the collection has `soft_delete` enabled. Now uses
  `resolve_trash()` for soft-delete collections, matching the gRPC
  and admin handlers.

- **Non-constant-time HMAC comparison in example** — The example
  `api_key_strategy.lua` compared HMAC signatures with `~=` (standard
  string comparison), enabling timing attacks. Now uses a double-HMAC
  pattern for constant-time comparison.

- **gRPC `UpdateMany` missing draft flag in validation** — The
  `ValidationCtx` for bulk updates did not pass the `draft` flag from
  the request. Draft bulk updates incorrectly enforced required-field
  validation, causing them to fail when required fields were omitted
  (which is allowed in draft mode).

- **Upload API leaked read-denied fields** — The upload POST and PATCH
  endpoints returned the full document in the response without
  stripping fields the user lacks read access to. Now applies
  `check_field_read_access` and removes denied fields before
  responding.

- **Lua `crap.globals.get`/`update` had no access control** (HIGH) —
  The Lua globals API bypassed all access control — no
  `overrideAccess` option, no collection-level checks, no field-level
  stripping. Any hook code could read and write all global data
  regardless of user permissions. Now supports `overrideAccess` option
  (default `false`), enforces collection-level read/update access, and
  strips field-level read/write-denied fields.

- **Lua `crap.globals.update` skipped validation** (HIGH) — Data
  written via `crap.globals.update()` bypassed all field validation
  (required, unique, length, numeric bounds, custom validators). Invalid
  data could be persisted directly. Now runs `validate_fields_inner`
  before writing.

- **Lua `crap.collections.create`/`update` leaked read-denied fields**
  — The returned document from Lua CRUD create and update operations
  included fields the user lacks read access to. The `find` and
  `find_by_id` functions correctly stripped these. Now all return paths
  (create, draft update, non-draft update) strip read-denied fields
  when `overrideAccess = false`.

- **`empty_trash` skipped lifecycle hooks** — The admin "Empty trash"
  action permanently deleted documents without running `BeforeDelete`
  or `AfterDelete` hooks. Hooks that perform cleanup side effects
  (cascade deletes, audit logging, external sync) were silently
  skipped. Now runs both hooks per document, matching the behavior of
  single delete and `DeleteMany`.

- **Windows build broken by Unix-only signal code** — `send_signal`,
  `is_process_running`, `stop`, `restart`, `status`, and
  `check_existing_pid` were not gated behind `#[cfg(unix)]`. The
  Windows CI build failed with unresolved `SIGKILL`/`SIGTERM` errors.
  All Unix-only functions and their call sites are now properly gated.
  On Windows, `--stop`/`--restart`/`--status` return a clear
  "not supported on this platform" error.

### Changed

- **`overrideAccess` default changed to `false`** (BREAKING) — All Lua
  CRUD functions (`find`, `find_by_id`, `create`, `update`, `delete`,
  `count`, `update_many`, `delete_many`, `undelete`) now enforce access
  control by default. Previously they bypassed access checks unless
  explicitly set to `false`. This follows the principle of least
  privilege — hooks that need unrestricted access must explicitly opt in
  with `overrideAccess = true`. Collections without access functions are
  unaffected (no restriction configured = allowed).

- **Responsive breakpoint raised to 1024px** — The mobile layout
  (hamburger sidebar, stacked edit layout, static headers) now activates
  at 1024px instead of 768px/900px. Two-sidebar layouts (nav + edit
  sidebar) were too cramped on tablets and small laptops.

- **Sticky subheader simplified** — Removed duplicate `ResizeObserver`
  (was in both `sticky-header.js` and `list-settings.js`), eliminated
  the `--list-header-height` CSS variable (redundant with
  `--sticky-header-bottom`), and removed direct inline style
  manipulation fallback on the edit sidebar. The sticky subheader now
  breaks out of `.main` padding with negative horizontal margins for
  edge-to-edge coverage, fixing content bleed visible during scroll.
  On mobile, headers revert to static document flow — no sticky
  positioning, no overlap issues.

- **Consistent chip styling** — Relationship chips and tag input chips
  now use the same visual style: primary-tinted background, medium font
  weight, rounded corners, and a remove button with red hover state.

- **Hardcoded colors replaced with CSS variables** — Bare `#fff` and
  `white` values in CSS and web components replaced with
  `var(--text-on-primary)` or `var(--bg-elevated)` for proper theme
  support.

- **Button disabled state** — `.button:disabled` now shows 50% opacity
  with `not-allowed` cursor. Input fields (`input:disabled`,
  `select:disabled`, `textarea:disabled`) show dimmed text, grayed
  background, and block pointer events.

- **Missing i18n keys** — Seven JavaScript translation keys
  (`search_to_add`, `search`, `are_you_sure`, `ok`, `documents`,
  `error`, `no_details`) were used in web components but missing from
  the `#crap-i18n` data island. Now included. Added `error` and
  `no_details` keys to en/de translation files.

- **Email template colors** — Password reset and email verification
  templates updated from `#2563eb` to `#1677ff` to match the system
  primary color.

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

- **Lua sandbox escape via `load()` / `loadstring()`** (CRITICAL): The
  Lua sandbox removed `loadfile` and `dofile` but not `load()` or
  `loadstring()`. A malicious hook could compile and execute arbitrary
  code with `load("os.execute('...')")()`, fully bypassing the sandbox.
  Now removes `load`, `loadstring`, `loadfile`, and `dofile`. Regression
  tests added for all four globals and a bypass attempt.

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

- **Upload serving path traversal hardening** (MEDIUM): The upload file
  serving endpoint relied solely on string-based `..`/`/`/`\` checks.
  Added canonicalization verification (`starts_with` on the canonical
  uploads directory) as defense-in-depth against symlink or encoding-based
  traversal vectors.

- **Upload file deletion path traversal hardening** (LOW): `delete_upload_files`
  joined document-stored URLs to the config directory without verifying the
  resolved path stayed within the uploads directory. A corrupted database
  record could cause arbitrary file deletion. Now canonicalizes and verifies
  the path stays within the uploads directory.

- **Lua package path injection** (MEDIUM): `setup_package_paths` interpolated
  the config directory path into a Lua code string without escaping. A
  directory name containing `"` or `\` could inject arbitrary Lua code.
  Replaced string interpolation with direct Lua API calls (`Table::set`).

- **PRAGMA table name validation** (LOW): `sqlite_get_table_columns` and
  `sqlite_get_table_column_types` interpolated table names into `PRAGMA
  table_info()` without validation. Added alphanumeric + underscore
  validation before PRAGMA execution.

- **MCP `safe_config_path` non-existent parent bypass** (LOW): When
  writing a file with a non-existent parent directory, `safe_config_path`
  skipped the canonicalization check entirely. Now walks up the parent
  chain to find the nearest existing ancestor and verifies it stays within
  the config directory.

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

- **Upload file cleanup silently swallowed DB errors** (HIGH): When
  deleting an upload-collection document, the pre-delete query to load
  file paths used `.ok().flatten()`, silently discarding database errors.
  If the query failed, upload files were never cleaned up — leaking disk
  space permanently. Now logs a warning on failure.

- **Globals used hardcoded default LocaleConfig** (MEDIUM): The global
  update path used `LocaleConfig::default()` for reference counting
  instead of the actual configured locale. This could cause incorrect
  ref count snapshots in projects with non-default locale settings. Now
  extracts the locale config from the input's locale context.

- **Dashboard exposed collection metadata without access checks** (MEDIUM):
  The admin dashboard showed document counts and last-updated timestamps
  for all collections and globals regardless of the user's read access.
  Now skips collections/globals the user cannot read.

- **Sidebar navigation ignored access control** (MEDIUM): The sidebar nav
  listed all collections and globals regardless of the user's read access.
  Added `filter_nav_by_access()` to all admin page handlers. The collection
  list page (`/admin/collections`) also now filters by read access.

- **Multipart form field parse failure produced silent empty string**
  (MEDIUM): If a form field failed to parse (e.g., network interruption),
  the error was logged but the field was silently set to an empty string.
  Optional fields would lose data without any user feedback. Now propagates
  the error as a proper form validation failure.

- **Fragile `unwrap()` after `is_some()` guard in validation** (MEDIUM):
  `validate_scalar_field` checked `ctx.locale_ctx.is_some()` then called
  `.unwrap()` on a separate line. Refactored to `if let Some(lctx)` for
  safety against future refactors.

- **Unsafe UTF-8 byte slicing in image status display** (MEDIUM): Image
  queue status used `&e.id[..n]` byte slicing for display truncation,
  which panics if the offset falls within a multi-byte character. Changed
  to `chars().take(n).collect()`.

- **Regex compiled on every config env-var substitution call** (LOW):
  `substitute_env_vars` compiled a new `Regex` on each invocation. Moved
  to a `LazyLock` static for one-time compilation.

- **`from_utf8_lossy` silently replaced invalid UTF-8 in SQLite results**
  (LOW): SQLite text column values were converted with `from_utf8_lossy`,
  silently replacing invalid bytes with the replacement character. Now logs
  a warning when invalid UTF-8 is encountered before falling back to lossy
  conversion.

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
