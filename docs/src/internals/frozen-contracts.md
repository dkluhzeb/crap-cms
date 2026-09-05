# Frozen Contracts

This page lists the parts of crap-cms that are a **permanent contract** as of
the stabilization release. Each is correct as-is — it is recorded here so a
future change is a deliberate, breaking decision rather than an accidental
"cleanup". Anything on this list can be **extended** (new variants, new keys)
but not renamed, removed, or reshaped without breaking existing users' configs,
stored data, or clients.

**Freeze policy during the alpha series:** every alpha release *targets* a
complete freeze of this page. If a contract nonetheless turns out to be wrong,
it is fixed **properly** — a clean breaking change with an upgrade-guide entry
and migration gate — never preserved through a compatibility workaround, and
the freeze target moves to the next alpha. The project moves to **beta** only
once an alpha cycle has passed with this page untouched; from beta on, the
freeze is unconditional.

## On-disk / storage (changing any = a data migration)

- **System column namespace.** All `_`-prefixed columns (`_status`,
  `_deleted_at`, `_ref_count`, `_order`, `_locale`, `_block_type`, the auth
  columns `_password_hash`/`_reset_token`/… , version columns
  `_parent`/`_version`/`_latest`/`snapshot`) and the non-prefixed `id` /
  `parent_id` / `created_at` / `updated_at`.
- **Companion/derived column suffixes.** `{field}_tz` (timezone), `{field}_lang`
  (code language). The suffixes are reserved from user field names.
- **Upload metadata columns.** `filename`, `mime_type`, `filesize`, `width`,
  `height`, `url`, `focal_x`, `focal_y`, plus per-size/format variants.
- **Naming schemes.** Group columns `group__field`, localized columns
  `field__locale` (and `group__field__locale`); join tables `{collection}_{field}`
  and `{collection}_{group}__{field}`; global tables `_global_{slug}`; version
  tables `_versions_{slug}`. Identifiers are capped at **63 bytes** (Postgres).
- **JSON storage shapes.** The blocks `data`-column split (`id` + `_block_type`
  hoisted out, everything else in `data`) — the `_block_type` discriminator key
  has one canonical source, `core::BLOCK_TYPE_KEY`, read through it by every
  block-aware surface (the one unavoidable literal, the admin `BlockRow` serde
  `rename`, is pinned to it by a test) — the "relational spine vs nested JSON"
  boundary (top-level array/blocks/relationship get join tables; anything nested
  inside a row is JSON), and the version-snapshot JSON shape (restore must read
  every snapshot ever written). A **group** nested in a row (array or block) is
  stored as a JSON **object** (`{…}`), not a one-element array — the block form
  parser selects a row's sub-field defs from its `_block_type` so the group is
  recognized as a single-object composite.
- **Timestamp write format is one ISO-8601 `…Z` shape on every backend.** Both
  the app-side clock (`utc_now()`, bound as a parameter) and the SQL "current
  time" expression (`DbConnection::now_expr()`, plus `date_offset_expr()` for job
  retry scheduling) produce `YYYY-MM-DDTHH:MM:SS.mmmZ`. This matters because
  timestamp columns are `TEXT` and compared **lexically** (sort keys, cursor
  pagination, `retry_after <= now`), and a column such as `updated_at` is written
  by both paths — a status change uses `now_expr()`, an ordinary edit binds
  `utc_now()`. SQLite must not fall back to `datetime('now')` (space separator,
  no millis/`Z`), which collates before the ISO form. Legacy rows written before
  this are normalized to ISO on read (`normalize_timestamp`).
- **Column types.** Timestamps and dates are `TEXT` (ISO-8601) on every backend;
  numbers are floating point (`REAL`/`DOUBLE PRECISION`); integers/flags are
  `BIGINT` on Postgres. Whole-valued numbers serialize back as JSON integers.
  A **scalar `has_many` list** (`Text`/`Number`/`Select`/`Radio` with `has_many`,
  i.e. `FieldDefinition::is_has_many_scalar`) is stored as a JSON array in a
  `TEXT` column regardless of the base type (a numeric column can't hold the
  array and Postgres rejects it). One `ColumnSpec::ddl_type` decides this for the
  CREATE and reconcile paths; the write edge canonicalizes each element to the
  field's type (`coerce_has_many_scalar`) and the read path parses it back
  (`parse_has_many_scalar`), so the list round-trips identically across surfaces.
- **System tables** (`_crap_meta`, `_crap_migrations`, `_crap_cron_fired`,
  `_crap_user_settings`, `_crap_jobs`) and the `_crap_meta` one-time-migration
  gate keys (`ref_count_backfilled`, …) — renaming a gate key re-runs the
  migration on every existing database. The gate **value** is the intended
  re-run lever; never rename the key.
- **Generated SQL identifiers are always quoted.** Every column/table name
  interpolated into generated SQL (CREATE/ALTER/INSERT/UPDATE/SELECT and FTS
  sync) goes through `quote_ident`, so a field named after a SQL reserved word
  (`order`, `select`, `group`, …) is valid on both backends. SQLite runs with the
  double-quoted-string misfeature disabled (`SQLITE_DBCONFIG_DQS_DDL`/`_DML`
  off), so a double-quoted token is unambiguously an identifier — a reference to
  a missing column errors rather than silently reading as the literal string.
  Never emit an unquoted user-derived identifier, and never re-enable DQS.

## Client-visible shapes

- **Returned document shape.** `id`, the field columns, `created_at`,
  `updated_at`. Localized fields under `locale = "all"` are a per-locale map
  (`{ en = .., de = .. }`); single-locale reads return the scalar.
- **Pagination object** (`result.pagination`): snake_case fields `total_docs`,
  `limit`, `page`, `has_next_page`, `has_prev_page`, `total_pages`, `page_start`,
  `prev_page`, `next_page`, `start_cursor`, `end_cursor`.
- **Result array key is `documents`** across `find` / `create_many` /
  `list_versions`. Bulk count keys: `created` / `modified` / `deleted` +
  `skipped`.
- **Polymorphic relationship read format** `"collection/id"`.
- **Filter DSL.** The operator set (`equals`, `not_equals`, `like`, `contains`,
  `greater_than`, `less_than`, `greater_than_or_equal`, `less_than_or_equal`,
  `in`, `not_in`, `exists`, `not_exists`) is the **one grammar every surface**
  speaks — the gRPC/JSON `where` API, the admin list URL, MCP, the Lua filter
  representation, and (since alpha.10) the access-constraint tables — all
  single-sourced through `FilterOp::op_name` / `FilterOp::scalar_from_name`
  and decoded by `decode_where_map`; the grammar's *description*
  (`FILTER_OP_SPECS`: names, value shapes, docs) is likewise single-sourced
  and pinned to the enum by a consistency test. An empty group inside `or`
  is a hard error on every surface (it would vacuously match every row). (Alpha ≤10 spelled the ordered operators
  differently per surface — the admin URL's terse `gt`/`gte`/`lt`/`lte` and MCP's
  `greater_than_equal`/`less_than_equal` — those short forms were removed in
  favor of the single verbose grammar.) Empty-`in` → no match / empty-`not_in` →
  all match, plus the dot-notation nested-path grammar. The lenient filter-value
  coercions (checkbox accepts `1/true/yes/on`; a non-numeric number filter falls
  back to text) are a **deliberate, permanent** leniency.
- **Cursor token format** (base64url JSON) — kept decodable for in-flight URLs.
- **Timestamp formats are deliberately per-concern and must not be "unified".**
  Persisted document date values normalize to millisecond ISO 8601 UTC
  (`YYYY-MM-DDTHH:MM:SS.000Z`, via `utc_now` / `normalize_date_value`). The
  event-stream payload `timestamp`, the Lua `now()` helper, and the export /
  backup / scaffold manifest timestamps are RFC 3339 (`to_rfc3339`), an outward
  contract for their consumers. The scheduler's cron-window dedup keys
  (`_crap_cron_fired`) are full-precision RFC 3339 compared only against each
  other. These serve distinct consumers; forcing one format would either break a
  wire contract or lose scheduler precision.

## Generated client types (`typegen client` / `typegen proto`)

These are the frozen output *shapes* of the code generators. The files are
regenerated by the user, but consumers write code against these shapes, so
changing a representation is a breaking change to every consumer.

- **Relationships are populate-aware and dual-form.** A relationship/upload
  field generates a type that is *either* an id string (`depth = 0`) *or* the
  populated document (`depth >= 1`): Rust `Rel<T>` (`#[serde(untagged)] enum {
  Doc(Box<T>), Id(String) }`), Go `Rel[T]` (struct + custom JSON), TypeScript
  `string | TDocument`, Python `str | T`; a has-many field is a list of that. Do
  not flatten either side back to a bare id.
- **A single (non-`has_many`) relationship is optional on read**, independent of
  the write-side `required` flag (it can be absent after soft-delete or
  access-deny). The optionality is part of the type.
- **`select` narrows to a named type, and Rust/Go are lossless.** Rust
  `enum { …, Other(String) }` (`serde(from/into)`), Go a `string` newtype with
  consts — both must preserve an unknown value, not reject it. TypeScript a
  string union, Python a `Literal`.
- **Polymorphic relationships are a discriminated type over their targets**
  (Rust untagged enum + a `#[serde(tag = "collection")]` ref enum, TS/Python a
  union of the target documents, Go `interface{}`) — never a bare string.
- **Per-collection types split `…Data` (writable) and `…Document` (adds `id` +
  timestamps); globals are a single type. `CollectionSlug` enumerates the
  slugs.**
- **Identifier safety and collision policy are frozen.** A name that collides
  with a language's rules is sanitized per language with the **wire key
  preserved** (never rename the wire key to fix a language identifier); a
  type-name collision is a hard generation error, not a silent rename.
- **Rust `typegen proto` decodes into the `typegen client -l rs` structs** — the
  two are one contract and must compile together, including decoding a populated
  relationship (`Rel::Doc`) nested at any depth.

## gRPC wire format (`proto/content.proto`)

- **Document values use the typed `FieldValue` / `DataMap` / `FieldList`
  messages**, not `google.protobuf.Struct`. `FieldValue` mirrors JSON but splits
  numbers into `int_value` (`int64`, exact) and `double_value`. A producer sets
  exactly one variant; an integer that fits `i64` uses `int_value`, a fractional
  value or an out-of-`i64` integer uses `double_value`. This is the frozen shape
  for every `data` / `fields` field — do not revert it to `Struct` (that would
  re-introduce the >2^53 rounding this replaced).
- **JSON-string escape hatches are intentional and permanent** — do NOT promote
  them to typed messages: `FindRequest.where` (a JSON filter string, so new
  operators need no wire change), `FieldInfo.type` (field-type name as a free
  string), and the job `data` / `result_json` payloads.
- **Schema introspection is a one-way lossy projection.** `DescribeCollection`
  flattens `tabs` sub-fields into `fields` (tab grouping is not reconstructable),
  and `FieldInfo.name` is the **Lua** field name (nested), never the flattened
  DB column (`group__sub`).
- **Enum defaults.** Every enum has an explicit `*_UNSPECIFIED = 0`. A value the
  server can't map collapses to `UNSPECIFIED` (e.g. a `cli`-scheduled run in
  `JobScheduledBy`, a non-`published`/`draft` version status) rather than
  erroring; adding an enum value is wire-safe, removing/renumbering is not.
- **Removed proto fields are compacted, not reserved.** While the wire format is
  pre-freeze (alpha), a removed field's tag is reclaimed by renumbering the
  survivors so the message stays gap-free. After the freeze, removed tags must
  instead be `reserved`.
- **The CRUD request messages are GENERATED — never hand-edit them.** Their
  field names, types, tags, and comments are pinned in the single-source wire
  spec (`service::op::wire_proto::PROTO_MESSAGES`, layered on the wire model
  `service::op::wire`); `cargo xtask gen-proto` renders them and `--check`
  gates CI. Tags are append-only by construction: renumbering or retyping a
  shipped field means editing the pinned spec, which the wire-parity tests
  and the regenerated diff both surface. Everything outside those messages
  (responses, auth, jobs, subscribe, the service block) stays hand-written.

## MCP (Model Context Protocol)

- **Tool-name grammar** `{op}_{slug}` for collections, `global_{op}_{slug}` for
  globals, plus the static tool names. Because `{op}` includes the compound
  forms `create_many_` / `update_many_` / `delete_many_` / `find_by_id_`, a
  collection/global slug **may not begin with `many_` or `by_id_`** — enforced at
  load (`reject_reserved_tool_prefix`).
- **Tool input schemas are strict on data keys.** Write tools reject a data key
  that is neither a reserved meta-key (`id`/`locale`/`draft`/`events`, and
  `password` on auth collections) nor a declared top-level field (layout wrappers
  are transparent). The `where` filter rejects unknown operators and malformed
  operator values loudly (never silently drops a clause).
- **JSON-RPC 2.0 conformance:** the `jsonrpc` member must be `"2.0"`; a request
  with no `id` is a notification and receives no response; error responses carry
  `id: null` when it can't be determined. Error codes are the standard set
  (`-32700` … `-32603`). Tool errors are returned in-band as a successful result
  with `isError: true`, not as a JSON-RPC error.
- **`read_config_file` redacts `crap.toml` secrets** (`auth.secret`,
  `email.smtp_pass`, `mcp.api_key`, S3 `secret_key`), matching the redacted
  `crap://config` resource. Secrets never leave the server through MCP.
- **MCP is a machine surface**: transport-authenticated (API key over HTTP,
  process-gated over stdio) and gated per-collection by the `access.mcp` key; it
  runs with `override_access`, so per-row/field access rules do **not** further
  restrict an authorized MCP caller.

## Auth — TOTP (RFC 6238)

- **Storage columns** `_totp_secret` / `_totp_confirmed` /
  `_totp_last_step` on an auth collection with `mfa = "totp"`, added by
  the ungated per-column migration.
- **At-rest secret format** (versioned `v1`): AES-256-GCM over
  `base64(12-byte nonce ‖ ciphertext)`, key =
  `SHA-256("crap-cms:totp-secret:v1" ‖ "\n" ‖ [auth] secret)`. Rotating
  `[auth] secret` permanently invalidates enrolled secrets — enrollment
  restarts (fail-closed, `error!`-logged).
- **Parameters**: SHA-1, 30-second step, 6 digits, ±1 step verification
  window; replay guarded by a monotonic `_totp_last_step` CAS.

## MCP HTTP sessions (`Mcp-Session-Id`)

- Identity-for-audit **only** — the API key still authenticates every
  request; a missing/unknown/expired session id is **never an error**
  (fail-soft). Caps: `IDLE_TTL = 30 min`, `MAX_SESSIONS = 1024`
  (oldest-evicted).

## Job trigger options (`delay` / `unique` / `priority`)

- `unique`: a colliding key returns the **existing** run's id (not an
  error); collision scope is **pending+running only** (partial unique
  index). `delay`: integer seconds or a duration string (`"5m"`);
  negative rejected. `_crap_jobs` carries `unique_key`, `priority`,
  `retry_after` columns beyond `data`/`result`/`error`/`scheduled_by`.

## Ranked FTS search (`order_by = "_rank"`)

- Requires a `search` term; rejected with cursor pagination; `-_rank` is
  a hard error; `_rank` is carved out of the `select`-valid and
  sortable-column sets. (Supersedes the earlier "ranked search removed"
  note.)

## Hooks

- **The Lua sandbox capability contract.** Hook code can never execute
  processes (`os.execute` and `io.popen` both removed), load code
  dynamically (`load`/`loadstring`/`loadfile`/`dofile` removed), or load
  native modules (`package.cpath` emptied, `package.loadlib` and
  `string.dump` removed); `os` is reduced to
  `clock`/`date`/`difftime`/`time`. The `io` file API is **deliberately
  available** — custom storage backends are documented as
  Lua-may-map-to-filesystem. The complete surviving global set is pinned
  by `sandbox_globals_match_reviewed_allowlist`; extending it is a
  reviewed decision, re-adding a removed capability is a breaking
  security change.
- **The 9 `HookEvent`s** and their per-operation firing order:
  field `before_validate` → richtext-attr `before_validate` → collection
  `before_validate` → validate → field `before_change` → collection
  `before_change` → persist → field `after_change` → collection `after_change` →
  registered `after_change`; reads run `before_read` → strip → `after_read`;
  deletes `before_delete` → `after_delete`. `before_render` is global-only.
- **Which events get CRUD access** (before/after-change, before/after-delete,
  field before-validate/before-change/after-change) vs which do not (`before_read`,
  `after_read`, `before_broadcast`, validators, conditions). `before_render` is the
  one **read-only** tier — see below.
- **Hook-return semantics.** Only `data` and `context` are read back; `data`
  **replaces** `ctx.data` wholesale. A normal hook returning `false` is ignored
  (only `error()` aborts); `before_broadcast`/live-filter returning `false`/`nil`
  suppresses. These asymmetric meanings are locked.
- **`ctx.operation` value set**: `create` / `update` / `delete` / `find` /
  `find_by_id` / `get` / `init` (hook context); access functions also see
  `trash` / `undelete` / `unpublish` / `restore` / `count` / `search` / `read` /
  `subscribe` / `trigger`. Hook/field-hook context key names are frozen.

### `before_render`

- **The signature is `fn(ctx, info)`.** `ctx` is the page context; `info` is the
  page identity (`page`, `template`, `collection?`, `global?`). `info.page` is
  the same discriminant as `ctx.page.type`, and both come from the generated
  [template-context reference](../admin-ui/reference/template-context.md) — a
  page's `page.type` value and template name are part of that frozen table.
- **One shared table.** Every registered hook is handed the same Lua table; the
  context is converted from and back to JSON exactly once per render regardless
  of hook count. A hook that returns a table replaces the context for the hooks
  after it; `nil` keeps the current one.
- **The access level is shared with `crap.template_data`.** Both render-time
  extension points on a page run under one [`RenderCrud`] — same identity,
  same database access — so neither can drift into being the privileged one.
- **Read-only on authenticated pages; no database at all on unauthenticated
  and error pages.** Reads run as the signed-in admin with normal access
  control. Writes and `crap.transaction(fn)` are refused — a page render must
  not take the write path (it would serialize admin page loads against writes)
  and must not be able to mutate. The auth/error carve-out is a security
  boundary: with no viewer there is no identity to scope a read by, and error
  pages have to render when the database is what failed.
- **Failure is always fail-soft.** A hook error, a non-table return, or a
  conversion failure logs a warning and renders the page with the context as it
  stood. A render is never failed by a hook.

[`RenderCrud`]: https://docs.rs/crap-cms/latest/crap_cms/hooks/lifecycle/enum.RenderCrud.html

## Read-surface invariants

- **Pagination limit and populate depth are clamped at every read surface**
  (Lua / gRPC / MCP / admin) via `apply_pagination_limits` (cap `max_limit`) and
  `min(max_depth)`. Any new read surface **must** apply the same clamps — an
  untrusted limit/depth must never reach the query layer unclamped. The main
  find path and the offset-only gRPC `ListJobRuns` both floor the limit at **1**
  (never `LIMIT 0`, never an unbounded `LIMIT -1`): the find path via
  `PaginationCtx::validate`, `ListJobRuns` via `PaginationCtx::resolve_limit`.
  **Version listing** floors its limit *and* offset inside the shared
  `service::versions::list_versions` (via `floor_optional_limit`, which lives in
  `db::query` so every surface and the service share one helper), so the Lua,
  gRPC, and MCP version listings all inherit the floor at one point.
- **An unknown locale string errors on every surface — it is never silently
  dropped.** `LocaleContext::from_locale_string` rejects a locale outside the
  configured set, and every intake (Lua / gRPC / MCP / admin forms + validate
  endpoints, via the admin `parse_request_locale` helper) surfaces that as a
  request error. Swallowing it into a `None` context is forbidden: a `None`
  context on a localized collection reads/writes the bare columns (`title`
  instead of `title__en`) — the classic wrong-column footgun.
- **Live-mutation streams resolve access through one shared path.** The gRPC
  `Subscribe` and admin SSE streams build their per-subscriber view/mode maps via
  `EventAccessMap::resolve` and enforce them per event via `EventGate::evaluate`
  — one construction point and one enforcement point. Both are fail-closed (an
  access hook that errors or a global returning a row-filter drops the view) and
  a new stream surface must reuse both, never re-derive the access mapping.

## Server-config posture (frozen defaults)

- **gRPC per-IP rate limiting is off by default** (`grpc_rate_limit_requests =
  0`) and keys on the raw TCP peer (no `X-Forwarded-For`). Live deployments set
  it explicitly; behind an L7 proxy set it at the proxy. Changing the default to
  non-zero would collapse all clients behind a proxy into one bucket.
- **Schema introspection is public by default** (`public_schema_introspection =
  true`): `ListCollections` / `DescribeCollection` need no auth. Operators set it
  `false` to require authentication. It never gates document data.
- **Static protective headers apply to every response.** `X-Frame-Options`,
  `X-Content-Type-Options: nosniff`, `Referrer-Policy`, `Permissions-Policy`, and
  (outside dev mode) HSTS are stamped on the full router — built-in admin routes
  **and** merged custom routes. Only the nonce-bound admin CSP is admin-only
  (custom routes render their own bodies and carry no nonce).

## Custom routes & admin responses

- **CSRF is enforced only on mutating methods** (POST/PUT/PATCH/DELETE).
  Declaring `csrf = true` on a custom route that answers only safe methods
  (GET/HEAD/OPTIONS) is rejected at load — a safe-method handler must not mutate
  state, and a route that mutates must declare a mutating method.
- **Admin list sort eligibility requires a real column.** A field is sortable
  only if it has a parent column (`has_parent_column()`); a has-many
  relationship/upload (no column) is rejected at the 400 param gate, never passed
  to the query layer.
- **Admin JSON/lazy-load endpoints return real HTTP status codes through shared
  helpers.** Version restore returns 403 on denial (not a silent redirect);
  back-references, the delete dialog, and empty-trash go through
  `json_not_found` / `json_forbidden` / `json_conflict` / `json_bad_request` /
  `json_server_error` (and `require_collection_json`), so a given error is one
  status code and one `{"error": …}` envelope across sibling endpoints — 404
  (unknown), 403 (denied), 409 (a referenced document blocking delete), 400
  (bad input), 500 (failure). Two deliberate exceptions: the relationship-search
  autocomplete always answers `200` with a JSON array (a missing collection is
  an empty list; a real DB failure is logged, not surfaced), and
  evaluate-conditions returns its `field → bool` map — `{}` on the error path,
  because its JS consumer iterates the body as that map, so an `{"error": …}`
  envelope would inject a bogus field name.
- **Response status is mapped from the typed `ServiceError`, never re-derived by
  matching the error's `Display` string.** The gRPC surface owns the canonical
  `ServiceError → tonic::Status` mapping; the HTTP upload-delete surface mirrors
  it with an explicit variant match. A backend-string probe survives only where a
  condition has no typed variant (e.g. MCP `read_global` detecting an
  uninitialized global's missing table), and there it is scoped to
  `ServiceError::Internal` so typed errors always propagate.
- **Invalid-locale policy is intentionally surface-dependent.** Machine APIs
  (gRPC / MCP / Lua) propagate an unparseable `locale` as an error. The admin
  *rendering* helper (`build_locale_template_data`) logs a warning and falls back
  to no locale context, so a hand-edited `?locale=` query param degrades the edit
  page to the default view rather than 500-ing it. The admin picker only ever
  emits configured locales, so this fallback is reachable only off the happy path.

## Access model

- **The access-key set** (`read`, `create`, `update`, `delete`, `trash`,
  `draft`, `versions`, `unlock`, `admin`, `mcp`) and the **fallback chains**
  (`draft ?? update`, `trash ?? update`, `versions ?? update`). Reads are a
  union of allowed views that downgrades rather than erroring. Changing a
  fallback target silently re-permissions every config that omits that key.
- **Constraint tables use the canonical `where` grammar.** A `Constrained`
  access result decodes through the same `decode_where_map` as every CRUD
  filter — scalar shorthand, operator tables, and `["or"]` groups included —
  with the leaf allowlist (equality/membership on flat own columns) applied
  recursively. Every failure is a fail-closed deny: an empty constraint
  table, a decode error, an empty `or` group, a disallowed operator.
- **`access.admin` gates admin-UI visibility uniformly.** Both the sidebar nav
  and the dashboard cards hide a collection/global the user can't `admin`, and
  the rule is evaluated under operation `"admin"` — the same value the route
  middleware passes — so a hook branching on `ctx.operation` behaves identically
  in the UI filter and the real gate.

## Auth tokens

- **Password policy is enforced at the service write chokepoint.** A `password`
  supplied to a `create` or `update` on an auth collection is validated against
  `[auth.password_policy]` inside the service create/update path, so every
  surface (Lua / gRPC / MCP / admin) and both single and bulk `create` are
  covered by one check that no surface can bypass. A write context that does not
  thread the configured policy falls back to `PasswordPolicy::default()` — never
  to no enforcement. `create_many` accepts a per-item policed password (distinct
  per document); `update_many` **rejects** a password, because it applies one
  value to many rows and must not broadcast a single credential. A violation
  surfaces as a structured `password` field validation error, rendered uniformly
  by every surface.
- **Email is matched case-insensitively everywhere it is an identity.** Account
  lookup (`find_by_email` = `LOWER(email) = LOWER(?)`), the per-account login and
  forgot-password rate-limit keys, and uniqueness on an `Email`-typed field all
  compare case-insensitively — one address is one account with one lockout bucket
  regardless of casing. Non-`Email` unique fields (slugs, codes) stay
  case-sensitive. Two guarantees keep this closed: an auth collection's `email`
  field **must** be `type = "email"` and `unique = true` (load error otherwise,
  so the type-scoped case-insensitive check always applies to the identity
  field), and every auth collection carries a `UNIQUE INDEX ON (LOWER(email))`
  backstop (partial — active rows only — under soft delete) so even a race past
  validation can't create case-variant duplicate accounts.
- **The `token_use` claim** partitions signed tokens into `session` (accepted by
  every authenticated surface — admin cookie/bearer, gRPC, upload serve) and
  `mfa_pending` (accepted only by the MFA-completion endpoint). Session
  validation rejects a non-`session` token, so an MFA-pending token can never
  authenticate a request. A token minted before the claim existed decodes as
  `session`. Never accept `mfa_pending` as a session, or MFA becomes bypassable.
- **Single-use security tokens are minted at one chokepoint.** Password-reset
  and email-verification tokens both come from `generate_security_token()` — a
  32-character nanoid. Any new single-use-token flow uses the same helper so the
  entropy length can't drift between flows. (Tokens are opaque; the length may
  only grow, never shrink.)

## Scheduler & jobs

- **`JobStatus` value set** `{pending, running, completed, failed, stale}`
  (lowercase) — stored in `_crap_jobs.status`, matched in SQL, and surfaced to
  clients. Adding a value is a forward-compat break for older cluster nodes
  (which parse an unknown status back to `pending`); renaming breaks stored rows.
- **`_crap_jobs` columns** are append-only-text: `data`/`result`/`error` are
  free TEXT, read positionally. `scheduled_by` is a free-form provenance string
  (`cron`/`hook`/`api`/`grpc`/`manual`/`system`…) clients may match on.
- **`_crap_cron_fired` dedup key** = bare `slug`, window encoded in the `fired_at`
  value; system pseudo-crons use `__`-prefixed slugs (`__retention_purge`), which
  is why user job slugs cannot start with `_`.
- **Delivery guarantee = at-least-once.** A job that times out, or whose worker
  crashes (heartbeat expires past `heartbeat_interval × 3`), is **requeued** and
  re-runs; an exhausted one goes terminal `stale`. **Handlers must be
  idempotent.** `max_attempts = retries + 1`.
- **Retry backoff curve** `min(2^(attempt-1) × 5, 300)` seconds — 5,10,20,…,300 —
  hardcoded, no config knob.
- **Cron** is UTC-only, does **not** catch up after downtime (missed runs are
  dropped), and coalesces multiple missed occurrences to one fire. Accepts 5-field
  (seconds prepended) or 6/7-field (leading field = seconds) expressions.
- **`[jobs]` / `[jobs.queues.<name>]` config keys** and the `Option<T>` tri-state
  (`None` = inherit default, `Some(0)` = operator-chosen unlimited/none) are
  frozen; `deny_unknown_fields` rejects typos. `auto_purge` defaults to 30 days;
  `auto_purge = false` disables it (an empty string is rejected, not a disable
  sentinel).

## Project layout / CLI

- **Discovery directories** `collections/` `globals/` `jobs/` `hooks/` and the
  `init.lua` entrypoint; the `crap` Lua global.
- **CLI subcommand + flag names, positional-arg order, and exit codes.**
  Every error exits **1** — that universal mapping is the contract; the only
  differentiated codes are `status --check` → 2 on warnings and `update check`
  → 1 when an update exists. (No other exit codes are reserved; scripts may
  treat any non-zero as failure.) `serve --only` accepts `admin`/`grpc`, with
  `api` kept as a backward-compatible alias of `grpc`. Machine output: `export`
  JSON envelope and `serve --json` (and `--json` is forwarded to the detached
  `serve`/`work` child).
- **Behavioral flag defaults** users' scripts/CI depend on: `logs --lines`
  defaults to 100, `fmt` with no path scans `templates/`, `jobs status --limit`
  / `images --limit` default 20, `jobs purge --older-than` 7d, `bench`
  iterations 10/5. Changing any silently changes what a script does.
- **Stdout/stderr split (machine contract).** Diagnostics — `cli::error` and
  `cli::warning` — go to **stderr**; all normal output (`success`/`info`/`hint`/
  `header`/`step`/`kv`/`kv_status`, tables, spinners) goes to **stdout**. A
  pipeline can `2>/dev/null` to drop diagnostics and parse stdout. `export`
  emits only JSON to stdout when piped.
- **Glyph vocabulary.** The status/output glyphs are frozen as Unicode/ASCII
  pairs: `✓`/`+` (success), `⚠`/`!` (warning), `✗`/`x` (error), `→`/`>` (info),
  `───`/`---` (bar), `?` (prompt). The ASCII fallback is a real contract — a
  script running under `CRAP_NO_UNICODE=1` sees the ASCII half.
- **Environment-variable accepted-value contract.** `CRAP_NO_UNICODE` and
  `CRAP_FORCE_UNICODE` enable on any **truthy** value — `1`/`true`/`yes`/`on`
  (case-insensitive, trimmed) — and are disabled otherwise. `CRAP_LOG_FORMAT`
  activates JSON on the exact value `json`. `_CRAP_DETACHED` is a **reserved**
  internal parent→child marker (presence-only; do not set it). `CRAP_CONFIG_DIR`
  mirrors `--config`.
- **Backup/export formats** are gated by a numeric `format_version` — the layout
  (`manifest.json` + `crap.db` + `uploads.tar.gz`; the export envelope) is frozen
  for a given version.

## Template formatter (`crap-cms fmt`)

CI gates on `crap-cms fmt --check`, so the formatter's exact output is a frozen
contract: changing a rule reformats every committed `templates/**.hbs` and every
downstream config that vendored templates.

- **Idempotent and content-preserving.** `format(format(x)) == format(x)`, and
  formatting never adds, drops, or reorders content — text, mustache
  expressions, raw bodies (`<script>`/`<style>`/`<pre>`/`<textarea>` and
  `{{{{raw}}}}`), and comments come through unchanged (whitespace aside). Both
  are property-test invariants; the content one is the guard idempotency alone
  can't provide.
- **Verbatim regions.** Raw-content element bodies, `{{{{raw}}}}` raw blocks, and
  comments (`<!-- -->` / `{{!-- --}}`) are emitted byte-for-byte — the final
  blank-line-collapse and trailing-whitespace-strip passes explicitly skip them,
  because whitespace there is significant.
- **Inline whitespace is rendering-preserving.** A whitespace run inside inline
  content collapses to a single space (never widened, never dropped between
  tokens), and directly-adjacent inline elements (`<a>x</a><a>y</a>`) stay on one
  line so no line break is introduced that would render as a space. *Residual:*
  an adjacent inline pair whose combined length exceeds the 100-char line limit
  is still split; keep whitespace explicit where it must render.
- **Best-effort on unbalanced nesting, never an error.** Handlebars legitimately
  opens/closes HTML tags across `{{#if}}`/`{{else}}` branches, so the linear
  token stream is expected to be unbalanced; the printer clamps depth
  (`saturating_sub`) rather than validating balance. It never rejects a template
  for HTML nesting.
- **Frozen rule tables.** The `BOOLEAN_ATTRS`, `VOID_TAGS`, and
  `RAW_CONTENT_TAGS` sets, the 2-space indent, the 100-char line limit (measured
  in **characters**), attribute order preserved (never sorted), single-quote
  fallback only when the value has a literal `"` or a triple-stash `{{{ }}}`, and
  the single-final-newline / one-blank-line-max policy — all load-bearing once
  frozen. An empty input formats to a single newline.

## Pre-alpha.10 design freezes (2026-09-03)

Decided under the "cleanest solution, break now" rule; each is frozen from
alpha.10 on:

- **Locale-locked writes error.** A non-default-locale write containing a
  non-localized field is a validation error naming the field — never a
  silent skip.
- **`has_many` lives inside `relationship`.** The top-level flag next to a
  `relationship` table is a load error (legacy `relation_to` keeps its flat
  flag).
- **Event vocabulary is six operations.** `create`, `update`, `delete`,
  `undelete`, `unpublish`, `restore` — on the proto enum, SSE payloads, and
  the Lua live/broadcast contexts. Lifecycle mutations never masquerade as
  `update`.
- **Auth strategies are transactional.** Commit on authenticate, rollback
  otherwise; failed attempts can never persist writes.
- **`select` is strict.** Unknown names error; valid = top-level field names
  + `id`/`created_at`/`updated_at`/`_status`.
- **`surfaces` is strict and `"all"` is the every-surface sentinel** (future
  surfaces included). Unknown names error.
- **No direct/public storage URLs.** Everything serves through `/uploads/…`;
  a bypass returns only as an explicit signed-URL design.
- **Search is a prefix filter.** The ranked FTS mode was removed as dead
  code; ranked search would return as an additive feature.

## Event timing & transaction-outcome effects (frozen 2026-09-03)

- **Mutation events are published only for committed writes.** Every write
  path queues events during its transaction and flushes them strictly after
  a successful commit — the pool-write envelope, job handlers (per-op
  transactions, flushed post-handler), and `crap.transaction(fn)`. A
  rolled-back write never emits an event.
- **`crap.tx.on_commit` / `crap.tx.on_rollback` contract.** Effects are hook
  refs plus JSON payloads. Registration is fail-closed (an unresolvable ref
  or unserializable payload fails the registering hook, and with it the
  transaction); execution is fail-open (an effect error is logged and
  skipped — the outcome is final). `on_commit` runs only after commit,
  `on_rollback` only after rollback; effects run *outside* the transaction
  in pool-mode with `ctx = { data, outcome }`. Registrations from hooks
  fired by nested CRUD attach to the outermost transaction.

## Queued bulk operations (frozen 2026-09-04)

- **`queue = true` response shape.** The count fields (`created`,
  `modified`, `deleted`, `soft_deleted`, `skipped`) are `0` and the document
  list is empty; `job_id` is present. Without `queue`, `job_id` is absent.
  The `result_json` summaries are frozen: `{"created":N}`,
  `{"modified":N}`, `{"deleted":N,"soft_deleted":N,"skipped":N}`.
- **Queued runs are queuer-scoped, override-wide.** A `_system_bulk` run
  is readable through `GetJobRun` by the identity that queued it, and by
  any override caller (which is how the MCP job tools read them — those
  return status/result/error only, never the payload). It never appears in
  `ListJobRuns`. Unparseable run data fails closed.
- **Identity is a reference, re-checked at execution.** Only the user id,
  auth collection, and session version are stored — never a user document —
  and the user is re-loaded when the run executes: a locked or deleted
  account, or a session-version bump (force-logout, password reset,
  unverify), abandons the run. Strategy-authenticated callers cannot queue
  (their identity may be synthetic and is not re-resolvable). Anonymous callers cannot queue (`UNAUTHENTICATED`), and `CreateMany`
  with `queue` plus any per-item password is `INVALID_ARGUMENT`.
- **Exactly one attempt.** `_system_bulk` runs are pinned to
  `max_attempts = 1` at insert, independent of `[jobs.queues.bulk]
  retries` — a retry could re-apply an already-committed batch.
- **The budget is enforced, not just reported.** A run that exceeds its
  queue `timeout` commits nothing: the batch aborts itself and the atomic
  transaction rolls back, so the recorded failure is truthful.
- **`_system_bulk` is a reserved system slug** (`SYSTEM_JOB_SLUGS`). User
  job slugs cannot collide — `validate_slug` rejects a leading underscore.
- **Queue-time capture.** `bulk_max_documents`, `hooks`, `draft`, `events`,
  `locale`, and `force_hard_delete` are snapshotted when the run is queued;
  later config changes do not affect a pending run.
- **Refused before it is stored.** The collection access gate and the
  document cap run at queue time; a denial or an over-limit batch is a
  synchronous error, not a queued run that fails later.
- **`CancelJobRun` cancels a not-yet-claimed run**, authorized by the same
  rule that governs reading it. A claimed run cannot be cancelled.
- **A finished run does not retain its request body.** The stored payload
  is reduced to the queueing identity once the run reaches a terminal
  status.

## MCP job tools (frozen 2026-09-04)

- **`[mcp] job_tools` is three-state** — `false` | `"read"` | `"all"`;
  `true` is rejected as ambiguous. Tier membership is frozen: `"read"` =
  `list_jobs` + `get_job_run` + `list_job_runs`; `"all"` adds
  `trigger_job`. Every tier is enforced at execution, not only in
  `tools/list`, and the `queue` argument on the bulk tools is advertised
  and accepted only from `"read"` up.
- **System-job runs stay hidden.** `_system_email` and
  `_system_image_convert` runs are not readable through the job tools
  (their payloads carry delivery tokens); `_system_bulk` is the sole
  exception, under the queuer-scoped rule above.

## Explicitly NOT frozen (carve-outs recorded before the alpha.10 tag)

Named here so later fixes are improvements, not breaking changes:

- **Queued-bulk failure classification and check granularity.** Whether a
  crashed run surfaces as `failed` or `stale`, the grace the scheduler's
  outer timer allows a self-limiting job, and how often a batch checks its
  deadline (currently between documents and once before commit) are
  implementation details. The contract is only: an over-budget run commits
  nothing and is recorded as a failure.
- **Live-event delivery granularity under load.** Sequence numbers already
  make delivery best-effort (a lagging subscriber drops events and detects
  the gap). Since alpha.10 the stream pumps implement burst coalescing —
  each sweep collapses queued events latest-wins per document — and that
  granularity remains explicitly non-contractual: a subscriber is guaranteed
  an event carrying each changed document's *latest* state, never every
  intermediate event. Further batching (subscriber grouping, windowing)
  stays within this contract.
- **Upload URL storage IS frozen — which made signed URLs additive.** The
  value stored in a document's `url` / `{size}_url` columns is the
  `/uploads/…` proxy path, permanently. The signed-URL scheme (shipped in
  alpha.10) signs at read/serve time and never changes stored values. Its
  wire contract is frozen: `?exp=<unix-seconds>&sig=<hex>` query parameters
  on the serve path; `sig` = HMAC-SHA256 over
  `"crap-cms:upload-url:v1\n{path}\n{exp}"` keyed by `[auth] secret`
  (the `v1` context is the versioning seam for any future scheme); a valid
  pair is a mint-time capability that serves without the per-document gate;
  anything less than valid falls through to normal cookie/Bearer resolution
  and never removes access. Minting: `crap.uploads.sign_url(url,
  expires_in?)`; `expires_in` is capped at 30 days (relaxing the cap later
  is additive; the cap itself is not a frozen minimum).
