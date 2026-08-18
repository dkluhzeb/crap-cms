# Frozen Contracts

This page lists the parts of crap-cms that are a **permanent contract** as of
the stabilization release. Each is correct as-is — it is recorded here so a
future change is a deliberate, breaking decision rather than an accidental
"cleanup". Anything on this list can be **extended** (new variants, new keys)
but not renamed, removed, or reshaped without breaking existing users' configs,
stored data, or clients.

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
  hoisted out, everything else in `data`), the "relational spine vs nested JSON"
  boundary (top-level array/blocks/relationship get join tables; anything nested
  inside a row is JSON), and the version-snapshot JSON shape (restore must read
  every snapshot ever written). A **group** nested in a row (array or block) is
  stored as a JSON **object** (`{…}`), not a one-element array — the block form
  parser selects a row's sub-field defs from its `_block_type` so the group is
  recognized as a single-object composite.
- **Column types.** Timestamps and dates are `TEXT` (ISO-8601) on every backend;
  numbers are floating point (`REAL`/`DOUBLE PRECISION`); integers/flags are
  `BIGINT` on Postgres. Whole-valued numbers serialize back as JSON integers.
- **System tables** (`_crap_meta`, `_crap_migrations`, `_crap_cron_fired`,
  `_crap_user_settings`, `_crap_jobs`) and the `_crap_meta` one-time-migration
  gate keys (`ref_count_backfilled`, …) — renaming a gate key re-runs the
  migration on every existing database. The gate **value** is the intended
  re-run lever; never rename the key.

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
  `in`, `not_in`, `exists`, `not_exists`), empty-`in` → no match / empty-`not_in`
  → all match, and the dot-notation nested-path grammar. The lenient filter-value
  coercions (checkbox accepts `1/true/yes/on`; a non-numeric number filter falls
  back to text) are a **deliberate, permanent** leniency.
- **Cursor token format** (base64url JSON) — kept decodable for in-flight URLs.

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
  string), and the job `data_json` / `result_json` payloads.
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

## Hooks

- **The 9 `HookEvent`s** and their per-operation firing order:
  field `before_validate` → richtext-attr `before_validate` → collection
  `before_validate` → validate → field `before_change` → collection
  `before_change` → persist → field `after_change` → collection `after_change` →
  registered `after_change`; reads run `before_read` → strip → `after_read`;
  deletes `before_delete` → `after_delete`. `before_render` is global-only.
- **Which events get CRUD access** (before/after-change, before/after-delete,
  field before-validate/before-change/after-change) vs which do not (`before_read`,
  `after_read`, `before_broadcast`, `before_render`, validators, conditions).
- **Hook-return semantics.** Only `data` and `context` are read back; `data`
  **replaces** `ctx.data` wholesale. A normal hook returning `false` is ignored
  (only `error()` aborts); `before_broadcast`/live-filter returning `false`/`nil`
  suppresses. These asymmetric meanings are locked.
- **`ctx.operation` value set**: `create` / `update` / `delete` / `find` /
  `find_by_id` / `get` / `init` (hook context); access functions also see
  `trash` / `undelete` / `unpublish` / `restore` / `count` / `search` / `read` /
  `subscribe` / `trigger`. Hook/field-hook context key names are frozen.

## Read-surface invariants

- **Pagination limit and populate depth are clamped at every read surface**
  (Lua / gRPC / MCP / admin) via `apply_pagination_limits` (cap `max_limit`) and
  `min(max_depth)`. Any new read surface **must** apply the same clamps — an
  untrusted limit/depth must never reach the query layer unclamped. This
  includes the gRPC `ListJobRuns` / `ListVersions` limits, which are floored at
  0 (a negative `limit` must never bind as an unbounded `LIMIT -1`).

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
- **Admin JSON/lazy-load endpoints return real HTTP status codes.** Version
  restore returns 403 on denial (not a silent redirect); back-references and
  evaluate-conditions return 404 (unknown), 403 (denied), or 500 (error) with
  their JSON body — never `200` with an error payload.

## Access model

- **The access-key set** (`read`, `create`, `update`, `delete`, `trash`,
  `draft`, `versions`, `unlock`, `admin`, `mcp`) and the **fallback chains**
  (`draft ?? update`, `trash ?? update`, `versions ?? update`). Reads are a
  union of allowed views that downgrades rather than erroring. Changing a
  fallback target silently re-permissions every config that omits that key.
- **`access.admin` gates admin-UI visibility uniformly.** Both the sidebar nav
  and the dashboard cards hide a collection/global the user can't `admin`, and
  the rule is evaluated under operation `"admin"` — the same value the route
  middleware passes — so a hook branching on `ctx.operation` behaves identically
  in the UI filter and the real gate.

## Auth tokens

- **The `token_use` claim** partitions signed tokens into `session` (accepted by
  every authenticated surface — admin cookie/bearer, gRPC, upload serve) and
  `mfa_pending` (accepted only by the MFA-completion endpoint). Session
  validation rejects a non-`session` token, so an MFA-pending token can never
  authenticate a request. A token minted before the claim existed decodes as
  `session`. Never accept `mfa_pending` as a session, or MFA becomes bypassable.

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
- **CLI subcommand + flag names, positional-arg order, and exit codes**
  (notably `status --check` → 2 on warnings, `update check` → 1 when an update
  exists). Machine output: `export` JSON envelope and `serve --json`.
- **Backup/export formats** are gated by a numeric `format_version` — the layout
  (`manifest.json` + `crap.db` + `uploads.tar.gz`; the export envelope) is frozen
  for a given version.
