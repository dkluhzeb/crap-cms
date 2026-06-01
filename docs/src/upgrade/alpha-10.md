# Upgrading to alpha.10

This guide covers the upgrade path from `alpha.9` to `alpha.10`. It
focuses on operator and plugin-author action items first; additive
features follow.

alpha.10 is a stabilization release: several Lua-facing contracts that
were loose or silently-wrong are tightened so they can freeze. Most
projects need no changes — the items below only bite definitions that
were already relying on ignored or malformed input.

## TL;DR

- **Replace your binary, restart.** DB schema migrations apply
  automatically; no manual SQL.
- **Plugin authors: option typos now error.** Every Lua CRUD option
  table rejects unknown keys. A previously-ignored typo (e.g.
  `overrideAcces`) now fails loudly. Fix any stray keys in your
  `crap.collections.*` / `crap.globals.*` / `crap.jobs.define` calls.
- **Plugin authors: drop option keys from bulk-op *queries*.** The
  `update_many` / `delete_many` query table only accepts `where`. Move
  `overrideAccess` / `locale` / `draft` to the options argument.
- **Plugin authors: `crap.hooks.register` rejects unknown events.** A
  typo'd event name is now an error instead of a silent no-op.
- **Check field names.** A field named `id` / `parent_id` /
  `created_at` / `updated_at`, or one starting with `_`, is now
  rejected at definition time (it always collided with a generated
  column — previously it crashed at migration or silently shadowed).

## Required action items

### 1. Remove unknown keys from Lua CRUD option tables

Every option table now rejects unrecognized keys
(`deny_unknown_fields`), matching the behavior the query tables already
had. This catches typos that previously defeated the option they were
meant to set — most dangerously a misspelled `overrideAccess`, which
silently left access control **on**.

```diff
  crap.collections.posts.create(data, {
-     overrideAcces = true,   -- silently ignored before; now an error
+     overrideAccess = true,
  })
```

Affected: the option arguments of `create`, `update`, `delete`,
`find_by_id`, `validate`, `undelete`, `unpublish`, `list_versions`,
`restore_version`, the `crap.globals` ops, and the config table of
`crap.jobs.define`.

### 2. Move bulk-op options off the query table

The `update_many` / `delete_many` query (2nd) argument previously
*declared* option keys it never read — `overrideAccess` / `locale` /
`draft` on `update_many`, and `overrideAccess` / `locale` on
`delete_many`. The effective values came from the options argument all
along. The query table now carries only `where`; pass the options in
the options argument.

```diff
  crap.collections.posts.update_many(
-     { where = { status = "draft" }, overrideAccess = true },
+     { where = { status = "draft" } },
      { status = "published" },
+     { overrideAccess = true }
  )

  crap.collections.posts.delete_many(
-     { where = { status = "archived" }, locale = "de" }
+     { where = { status = "archived" } },
+     { locale = "de" }
  )
```

`delete_many`'s `locale` now lives in the options argument, so single
and bulk deletes take locale the same way.

### 3. Fix unknown hook event names

```diff
- crap.hooks.register("on_change", fn)   -- not a real event; now errors
+ crap.hooks.register("before_change", fn)
```

The valid events are listed under
[crap.hooks](../lua-api/hooks.md#events). An unrecognized name
previously logged a warning and registered a hook list that never
fired; it is now a hard error so the typo surfaces immediately.

### 4. Rename reserved field names

A field `name` is rejected at definition time when it collides with an
automatically generated column:

- starts with `_` (reserved for system columns), or
- contains `__` (reserved for group-field column nesting), or
- is exactly `id`, `parent_id`, `created_at`, or `updated_at`.

These names always collided with a generated column — they either
failed the `CREATE TABLE` with a duplicate-column error or silently
shadowed a system column. Rename the field.

Collection **slugs** are also checked for collisions with generated
join-table names at startup: a collection slugged `posts_tags`
conflicts with the `tags` array field of a `posts` collection (both
generate a `posts_tags` table). Boot fails with a clear error instead
of one definition silently corrupting the other's table during
migration.

### 5. Remove `[live] default_mode` from `crap.toml`

This key never did anything — every collection's live mode defaulted to
`metadata` regardless of it. Set the mode per collection instead.

```diff
  [live]
  enabled = true
- default_mode = "full"
  transport = "memory"
```

```lua
-- per-collection live mode (the only control):
crap.collections.define("posts", {
    live = { mode = "full" },
})
```

If present, config load now fails with `unknown field "default_mode"`.

## gRPC clients (regenerate from `proto/content.proto`)

Wire-contract changes — regenerate your gRPC stubs and adjust:

- **Removed always-true `success` fields** from `DeleteResponse`,
  `ForgotPasswordResponse`, `ResetPasswordResponse`, `VerifyEmailResponse`,
  and `AccountActionResponse`. A non-error response is the success signal;
  drop any `if (!resp.success)` branches. `DeleteResponse.soft_deleted`
  remains.
- **Removed `JobDefinitionInfo.handler`** (the internal Lua function
  reference) from `ListJobs`.
- **`JobRunInfo` is now the shared job-run message.** `ListJobRuns` returns
  `repeated JobRunInfo`; `GetJobRun` returns `GetJobRunResponse { run }`
  wrapping a `JobRunInfo` (was a flat `GetJobRunResponse`). Read a single
  run via `response.run`.
- **Closed-set string fields became enums:** `MutationEvent.operation` /
  `.target`, `VersionInfo.status`, `JobRunInfo.status` / `.scheduled_by`,
  and the `ListJobRunsRequest.status` filter. Use the generated enum
  accessors (`event.operation()` etc.); the zero value is `*_UNSPECIFIED`,
  which for the `ListJobRunsRequest.status` filter means "all statuses".
- **Account RPCs now return `UNAUTHENTICATED` before any collection-shape
  error.** A client calling `LockAccount`/`VerifyAccount`/etc. without (or
  with an invalid) token now gets `UNAUTHENTICATED` even when the
  collection is unknown or lacks `verify_email` — previously it could get
  `NOT_FOUND` / `INVALID_ARGUMENT` / `FAILED_PRECONDITION` first. Authenticate
  before relying on those shape errors.
- **Additive:** `CountRequest.trash` counts soft-deleted documents
  (mirrors `FindRequest.trash`). Non-breaking.
- **Doc-only:** `Create`/`Update` now document that a UNIQUE-constraint
  conflict maps to `ALREADY_EXISTS` (the runtime mapping was already
  `ALREADY_EXISTS`; only the proto comment was stale).

## Bug fixes (no action needed)

- **`crap-cms serve --only grpc`** is now accepted (matching the
  `[server] grpc_*` config keys). `--only api` still works as an alias,
  so no script changes are required.
- **`auth.password_policy` validation error** now names the real config
  path (it previously referred to a non-existent `auth.password.*` key).
- **Config backend selectors** (`[database] backend`, `[upload] storage`,
  `[email] provider`, `[cache] backend`, `[auth] rate_limit_backend`,
  `[live] transport`) are now validated at config load — a typo'd value
  fails immediately with the list of valid values instead of at server
  startup. Valid values are unchanged, so no config edits are needed.

- **Lua `find` / `find_by_id` honor `[depth] max_depth`.** Relationship
  population depth was previously clamped to a hardcoded maximum of 10,
  ignoring the configured ceiling. It now clamps to `[depth] max_depth`,
  matching the gRPC read path. Deployments with a `max_depth` other than
  10 will see the configured value take effect on the Lua surface.
- **No more spurious orphan-column warning for MFA collections.** The
  `_mfa_code` / `_mfa_code_exp` columns are now recognized as system
  columns, so migrations no longer log a false "column exists but is not
  in the Lua definition" warning for them.
- **MCP `delete` no longer crashes on localized upload collections.**
  The MCP delete tool now passes the configured locale to the service
  layer, matching gRPC and admin.
- **Reference counting now recurses into nested relationships.** A
  relationship nested inside a group within an array, a group within a
  block, or a has-many relationship inside a block was not counted
  toward delete-protection — so a referenced document could be
  hard-deleted while still in use, and counts could drift. All nesting
  depths are now counted. Existing databases recompute their
  `_ref_count` values once on the next startup (the backfill is
  version-gated); no action needed.
- **MCP hard-delete now cleans up upload files.** The MCP `delete` /
  `delete_many` tools now delete a removed document's uploaded files,
  matching gRPC and admin. (Soft-deletes still keep the files.)

## Behavior changes (likely no action)

- **Whole-valued `number` fields now serialize as integers.** Because
  `number` is stored as floating-point, an integer round-tripped through
  the database as `42.0` and was emitted that way on every read surface
  (REST/Lua/MCP/admin). Whole values now serialize as `42`; genuine
  fractions are unchanged (`42.5` stays `42.5`). JSON treats `42` and
  `42.0` as the same number, so virtually all clients are unaffected —
  the only thing that changes is a consumer that *string-matched*
  `"42.0"`, which will now see `"42"`. (gRPC is unaffected — it always
  carried numbers as `double`.)

- **JSON-decoded floats now keep full precision.** crap-cms now uses a
  correctly-rounded JSON float parser, so a `number` value read back
  from a JSON-backed path — the `blocks` `data` column, MCP arguments,
  keyset pagination cursors — is bit-identical to what was written.
  Previously the parser could be off by up to one ULP for some
  magnitudes (very large/small exponents). This only makes values *more*
  exact, so no action is needed. (gRPC is unaffected — it carries
  numbers as protobuf `double`, not JSON.)

## Additive features (alpha.10)

### MCP writes accept `locale`, `draft`, and `force_hard_delete`

The MCP `create` / `update` / `update_many` tools now accept a `locale`
and (for single create/update) `draft` argument, `delete` accepts
`force_hard_delete`, and `global_read` / `global_update` accept
`locale` — so localized and draft content is reachable over MCP,
matching the gRPC and Lua write surfaces. These are reserved top-level
arguments, excluded from the document's field data like the existing
`id` / `password`. See [MCP overview](../mcp/overview.md).

### Validate without persisting, on every surface

Collections and globals can now be validated without writing:

- **Collections** — gRPC `Validate` (existing), Lua
  `crap.collections.validate`, and the MCP `validate_<collection>` tool.
- **Globals** — newly added: gRPC `ValidateGlobal`, Lua
  `crap.globals.validate`, and the MCP `global_validate_<global>` tool.
  Global validation previously existed only in the admin UI.

All run the full before-write pipeline (coercion, validators, unique
checks, `before_validate` hooks) and return per-field errors. Globals
always validate in update mode against their singleton row.

### `number` fields accept `integer = true`

A `number` field can be restricted to whole values:
`{ type = "number", integer = true }`. Fractional input is rejected at
validation and the admin renders an integer stepper. Storage is
unchanged (floating-point — no migration), and whole values already
serialize as integers (see *Behavior changes* above). Composes with
`min` / `max` / `has_many`.

### gRPC `CountRequest.trash`

`Count` can now count soft-deleted (trashed) documents via a `trash`
flag, mirroring `FindRequest.trash`.

## Reference

- `CHANGELOG.md` at the project root — the full alpha.10 entry with
  every change.
- [crap.collections](../lua-api/collections.md)
- [crap.hooks](../lua-api/hooks.md)
- [Fields overview](../fields/overview.md#reserved-field-names)
