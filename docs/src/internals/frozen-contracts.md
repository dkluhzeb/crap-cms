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
  every snapshot ever written).
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
  untrusted limit/depth must never reach the query layer unclamped.

## Access model

- **The access-key set** (`read`, `create`, `update`, `delete`, `trash`,
  `draft`, `versions`, `unlock`, `admin`, `mcp`) and the **fallback chains**
  (`draft ?? update`, `trash ?? update`, `versions ?? update`). Reads are a
  union of allowed views that downgrades rather than erroring. Changing a
  fallback target silently re-permissions every config that omits that key.

## Project layout / CLI

- **Discovery directories** `collections/` `globals/` `jobs/` `hooks/` and the
  `init.lua` entrypoint; the `crap` Lua global.
- **CLI subcommand + flag names, positional-arg order, and exit codes**
  (notably `status --check` → 2 on warnings, `update check` → 1 when an update
  exists). Machine output: `export` JSON envelope and `serve --json`.
- **Backup/export formats** are gated by a numeric `format_version` — the layout
  (`manifest.json` + `crap.db` + `uploads.tar.gz`; the export envelope) is frozen
  for a given version.
