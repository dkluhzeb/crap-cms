# crap.collections

Collection definition and runtime CRUD operations.

## crap.collections.define(slug, config)

Define a new collection. **Init-only:** call this from
`collections/*.lua`, `init.lua`, or any file loaded by `require` from
those — i.e. while the `InitPhase` marker is set on the VM. Runtime
calls (from a hook callback firing during a request) error with:

> `crap.collections.define must be called from a definition file or
> init.lua. To change a registered collection, edit the file and
> restart the process.`

Schema changes need the migration runner and route wiring to re-run,
which only happens at startup; runtime registration would land in the
in-memory registry without the matching table, admin route, or
scheduler enrollment, so the call is rejected outright. (Mirrors the
behaviour `crap.richtext.register_node` has had since inception.)

```lua
crap.collections.define("posts", {
    labels = { singular = "Post", plural = "Posts" },
    fields = {
        crap.fields.text({ name = "title", required = true }),
    },
})
```

See [Collection Definition Schema](../collections/definition-schema.md) for all config options.

## crap.collections.config.get(slug)

Get a collection's current definition as a Lua table. The returned table is round-trip
compatible with `define()` — you can modify it and pass it back, **inside init**.

Returns `nil` if the collection doesn't exist.

```lua
-- inside a definition file or init.lua
local def = crap.collections.config.get("posts")
if def then
    -- Add a field
    def.fields[#def.fields + 1] = crap.fields.text({ name = "extra" })
    crap.collections.define("posts", def)
end
```

## crap.collections.config.list()

Get all registered collections as a slug-keyed table. Iterate with
`pairs()`. The realistic plugin pattern — bulk-attach a hook or field
across every collection — runs from `init.lua` (or a file it
`require`s) where the strict guard on `define` doesn't fire.

```lua
-- inside init.lua / a plugin loaded by init.lua
for slug, def in pairs(crap.collections.config.list()) do
    if def.upload then
        -- Add alt_text to every upload collection
        def.fields[#def.fields + 1] = crap.fields.text({ name = "alt_text" })
        crap.collections.define(slug, def)
    end
end
```

See [Plugins](../plugins/overview.md) for patterns using these functions.

## Runtime operations — `crap.collections.<slug>`

Every collection registered via `define()` gets a typed accessor at
`crap.collections.<slug>` exposing the full CRUD surface. The slug
is bound; return values are typed against the per-collection
`crap.doc.X` / `crap.find_result.X` classes — full IDE narrowing
without `---@type` ceremony.

All operations below are **only available inside hooks with
transaction context**.

> **Dynamic-slug dispatch.** For the rare case where the slug isn't
> known until runtime (auth strategies handed `context.collection`,
> a plugin iterating `crap.collections.config.list()`, the down-side
> of a migration), the same operations are reachable as
> `crap.collections.<method>(slug, ...)` — identical semantics, slug
> as the first arg. Use the slug-keyed form only when you genuinely
> don't have a string literal.
>
> Slugs that would shadow a method name on `crap.collections` (e.g.
> a collection literally named `"find"`) fail startup with a clear
> error. Rename the collection.

### `crap.collections.<slug>.find(query?)`

Find documents matching a query. Returns a typed result with
`documents` (array of `crap.doc.<Slug>`) and `pagination`.

```lua
local result = crap.collections.posts.find({
    where = { status = "published", title = { contains = "hello" } },
    order_by = "-created_at",
    limit = 10,
    page = 1,
    depth = 1,
})

for _, doc in ipairs(result.documents) do
    print(doc.id, doc.title)   -- doc: crap.doc.Posts
end
```

`result.pagination` carries `totalDocs`, `limit`, `page`, `totalPages`,
`pageStart`, `hasNextPage`, `hasPrevPage`, `prevPage`, `nextPage`,
`startCursor`, `endCursor` (cursor fields only when
`[pagination] mode = "cursor"` in `crap.toml`).

**Query fields:**

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `where` | table | `{}` | Field filters. See [Filter Operators](filter-operators.md). Supports `["or"]` for OR groups. |
| `order_by` | string | `nil` | Sort field. Prefix with `-` for descending. |
| `limit` | integer | `nil` | Max results to return. |
| `page` | integer | `1` | Page number (1-based). Converted to offset internally. |
| `offset` | integer | `nil` | Number of results to skip (alias for `page`). |
| `after_cursor` | string | `nil` | Forward cursor from a previous `result.pagination.end_cursor`. Mutually exclusive with `page`/`offset`/`before_cursor`. Cursor-mode only. |
| `before_cursor` | string | `nil` | Backward cursor from a previous `result.pagination.start_cursor`. Mutually exclusive with `page`/`offset`/`after_cursor`. Cursor-mode only. |
| `depth` | integer | `0` | Population depth for relationship fields. |
| `select` | string[] | `nil` | Fields to return. `nil` = all fields. Always includes `id`. When specified, `created_at`/`updated_at` are included only if explicitly listed. |
| `draft` | boolean | `false` | Include draft documents (versioned collections with `drafts = true`). |
| `trash` | boolean | `false` | Return only soft-deleted documents (collections with `soft_delete = true`). |
| `locale` | string | `nil` | Locale code for localized fields. |
| `override_access` | boolean | `false` | Bypass collection-level and field-level access checks. |
| `search` | string | `nil` | FTS5 full-text search query. |

### `crap.collections.<slug>.find_by_id(id, opts?)`

Find a single document by ID. Returns the typed document or `nil`.

```lua
local doc = crap.collections.posts.find_by_id("abc123")
if doc then
    print(doc.title)
end

-- With population depth
local doc = crap.collections.posts.find_by_id("abc123", { depth = 2 })

-- With field selection
local doc = crap.collections.posts.find_by_id("abc123", { select = { "title", "status" } })
```

**Options:** `depth`, `select`, `draft`, `trash`, `locale`, `override_access` — same semantics as `find`. `trash = true` looks the document up among soft-deleted rows (collections with `soft_delete = true`).

### `crap.collections.<slug>.create(data, opts?)`

Create a new document. Returns the created typed document.

```lua
local doc = crap.collections.posts.create({
    title = "New Post",
    slug = "new-post",
})
print(doc.id)  -- auto-generated nanoid

-- Create as draft (versioned collections only)
local draft = crap.collections.articles.create({
    title = "Work in progress",
}, { draft = true })
```

**Options:**

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `locale` | string | `nil` | Locale code for localized fields. |
| `draft` | boolean | `false` | Create as draft. Skips required-field validation (versioned + `drafts = true`). |
| `override_access` | boolean | `false` | Bypass collection/field access checks. |
| `hooks` | boolean | `true` | Run lifecycle hooks. Set `false` to skip all hooks and validation; DB write still runs. |
| `events` | boolean | `true` | Emit a live-update event for the created document. Set `false` for a quiet write (e.g. seeding/migrations). |

#### Auth collections

For collections with `auth = true`, the `password` field is handled
automatically: on create/update it's extracted before hooks run,
validated against `[auth.password_policy]`, hashed with Argon2id, and
stored in the hidden `_password_hash` column. Hooks never see the raw
password. On update, leaving `password` out (or empty) keeps the current
hash. The policy check runs at the service write chokepoint, so it
applies to `create`, `update`, and `create_many` alike (a weak password
is rejected here too, not only on the gRPC/admin surfaces) — matches the
gRPC/MCP/admin behavior.

### `crap.collections.<slug>.update(id, data, opts?)`

Update an existing document. `data` is a partial payload — only the
fields being changed need to be present. Returns the updated typed
document.

```lua
local doc = crap.collections.posts.update("abc123", {
    title = "Updated Title",
})

-- Draft update: saves a version snapshot only, main table unchanged
crap.collections.articles.update("abc123", {
    title = "Still editing...",
}, { draft = true })
```

**Options:**

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `locale` | string | `nil` | Locale code for localized fields. |
| `draft` | boolean | `false` | Version-only save. Creates a draft version snapshot without touching the main table. |
| `unpublish` | boolean | `false` | Set status to `draft` and create a draft version snapshot. Ignores `data` when unpublishing. Versioned collections only. |
| `override_access` | boolean | `false` | Bypass collection/field access checks. |
| `hooks` | boolean | `true` | Run lifecycle hooks. |
| `events` | boolean | `true` | Emit a live-update event for the updated document. Set `false` for a quiet write. |

### `crap.collections.<slug>.delete(id, opts?)`

Delete a document. Returns `true` on success. For collections with
`soft_delete = true` this moves the document to trash by default;
upload collections clean up their files on permanent delete (not
soft delete).

```lua
-- Soft-delete (moves to trash if the collection has soft_delete)
crap.collections.posts.delete("abc123")

-- Force permanent delete even on soft-delete collections
crap.collections.posts.delete("abc123", { force_hard_delete = true })

-- Bypass access control for internal operations
crap.collections.posts.delete("abc123", { override_access = true })
```

**Options:**

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `override_access` | boolean | `false` | Bypass `access.trash` (soft) or `access.delete` (permanent). |
| `hooks` | boolean | `true` | Run `before_delete` / `after_delete` hooks. |
| `force_hard_delete` | boolean | `false` | Skip `soft_delete` and remove the row permanently. Still requires `access.delete` when `override_access = false`. |
| `events` | boolean | `true` | Emit a live-update event for the deleted document. Set `false` for a quiet delete. |

### `crap.collections.<slug>.undelete(id)` (and others)

Restore a soft-deleted document from trash. Returns `true` on
success. Only available on collections with `soft_delete = true`.
Re-syncs the FTS index after undelete.

```lua
crap.collections.posts.undelete("abc123")
```

The accessor also covers `unpublish(id, opts?)`, `validate(data, opts?)`,
`count(query?)`, `create_many(items, opts?)`, `update_many(query, data,
opts?)`, `delete_many(query, opts?)`, `list_versions(id, opts?)`,
`restore_version(id, version_id, opts?)`, and `ref_count(id)` —
same shape as the slug-keyed equivalents, slug bound.

`unpublish` accepts `override_access`, `hooks`, and `events` options
(`events = false` for a quiet unpublish, matching
`update{ unpublish = true, events = false }`).

`delete_many` accepts a `trash = true` option that permanently removes
**already-soft-deleted** rows (empty the trash) — a hard delete of
trashed documents gated by `access.delete`. Without it, `delete_many`
never touches trashed rows. `ref_count(id)` is gated by read access: it
errors for a document the current user cannot read.

## Typing factories — `crap.collections.<slug>.{hook,field_hook,condition,access,...}`

Per-collection **typing helpers** that wrap your function literal
and let LuaLS infer the parameter types of the body. Pure
pass-throughs at runtime (`f(fn) = fn`); the typing comes from the
factory's signature. See [`crap.any`](typing-factories.md) for the
cross-collection equivalents.

Prerequisite: `"type.inferParamType": true` in `.luarc.json` (set
by default in the `init` scaffold).

### `crap.collections.<slug>.hook(fn)`

Collection lifecycle hook with `context` narrowed to
`crap.hook.<Pascal>`. Use for `before_validate`, `before_change`,
`after_change`, `before_read`, `after_read` — events where the
runtime ships a typed context.

```lua
-- hooks/inquiries/notify.lua
return crap.collections.inquiries.hook(function(context)
    -- context.data is typed crap.data.Inquiries
    if context.operation == "create" then
        crap.jobs.queue("notify", { email = context.data.email })
    end
    return context
end)
```

For `before_delete` / `after_delete` / `before_broadcast` the
runtime always sends a generic `crap.HookContext` — use
`crap.any.collection_hook(fn)` instead.

### `crap.collections.<slug>.field_hook(field, fn)` and `field_hook(fn)`

Field-level hook. Two forms:

```lua
-- Per-field: value narrows to the field's declared type
return crap.collections.posts.field_hook("title", function(value, context)
    -- value: string (posts.title is a text field)
    return value:lower()
end)

-- Any field: value is `any`, context still narrowed per-collection
return crap.collections.posts.field_hook(function(value, context)
    -- context: crap.field_hook.Posts
    return value
end)
```

The per-field form uses an `---@overload` per field declared on the
collection plus a `string` fallback for dynamic names. `value`'s
type matches the field type the typegen emits — string for text /
textarea / email / date / richtext / code, number for number,
boolean for checkbox, the literal-string union for select / radio,
`string` or `string[]` for relationship / upload, etc.

### `crap.collections.<slug>.condition(fn)`

Display condition with `data` narrowed to `crap.data.<Pascal>`.

```lua
-- hooks/posts/show_external_url.lua
return crap.collections.posts.condition(function(data)
    -- data.post_type narrowed to its select union
    return { field = "post_type", ["in"] = { "link", "video" } }
end)
```

### `crap.collections.<slug>.access(fn)`

Access control function. Same signature as
[`crap.any.access(fn)`](typing-factories.md) — included on the
per-collection accessor for discoverability via
`crap.collections.<slug>.<TAB>`. No per-collection narrowing
(access context is uniform across collections).

```lua
return crap.collections.users.access(function(context)
    return context.user ~= nil
end)
```

### `crap.collections.<slug>.auth_strategy(fn)`

Custom auth strategy `authenticate` callback. Same shape as
`crap.any.auth_strategy(fn)`; lives on the per-collection accessor
for discoverability when scaffolding a strategy specifically for
that collection.

### `crap.collections.<slug>.row_label(fn)`

Computed row label for array/blocks fields. Receives the row table
and returns the display string (or `nil` to fall back to
`label_field`). Per-field narrowing of the row type is a future
enhancement — today the row is typed as `table<string, any>`.

## Slug-keyed dispatch (dynamic case)

For the rare case where the slug isn't known until runtime — auth
strategies handed `context.collection`, plugins iterating
`crap.collections.config.list()`, migration cleanup loops — use the
slug-keyed equivalents:

```lua
crap.collections.find(collection, query)
crap.collections.find_by_id(collection, id, opts)
crap.collections.create(collection, data, opts)
-- ... etc
```

Same semantics, slug as the first arg. Reach for these only when
you genuinely don't have a string literal — they don't narrow.

## Lifecycle Hooks in Lua CRUD

Lua CRUD operations run the **same lifecycle hooks** as the gRPC API and admin UI:

- **`create`**: before_validate → validate → before_change → DB insert → after_change
- **`update`**: before_validate → validate → before_change → DB update → after_change
- **`update_many`**: per-document: before_validate → validate → before_change → DB update → after_change
- **`delete`**: before_delete → DB delete → upload file cleanup → after_delete
- **`delete_many`**: per-document: before_delete → DB delete → upload file cleanup → after_delete
- **`find` / `find_by_id`**: before_read → DB query → after_read

All hooks have full CRUD access within the same transaction.

### Hook Depth & Recursion Protection

When hooks call CRUD functions that trigger more hooks, the system tracks recursion depth
via `ctx.hook_depth`. This prevents infinite loops:

- Depth starts at 0 for gRPC/admin operations, 1 for Lua CRUD within hooks
- When depth reaches `hooks.max_depth` (default: 3, configurable in `crap.toml`), hooks
  are automatically skipped but the DB operation still executes
- Use `ctx.hook_depth` in hooks for manual recursion decisions

```toml
# crap.toml
[hooks]
max_depth = 3   # 0 = never run hooks from Lua CRUD
```

```lua
function M.my_hook(ctx)
    if ctx.hook_depth >= 2 then
        return ctx  -- bail early to avoid deep recursion
    end
    crap.collections.audit.create({ action = ctx.operation })
    return ctx
end
```

### Skipping Hooks

Pass `hooks = false` to any write CRUD call to skip all lifecycle hooks:

```lua
-- Create without triggering any hooks
crap.collections.logs.create({ message = "raw insert" }, { hooks = false })
```

## Access Control in Hooks

By default, all Lua CRUD functions **enforce access control** (`override_access = false`). This follows the principle of least privilege — if your hook needs to bypass access checks, it must explicitly opt in with `override_access = true`.

> **Breaking change (0.1.0-alpha.3):** The default was changed from `true` to `false`. If you have hooks that call CRUD functions without specifying `override_access`, they now enforce access control. Add `override_access = true` to restore the old behavior.

When `override_access` is `false` (the default), the function enforces the same access rules as the external API:

- **Collection-level access** — the relevant access function is called with the authenticated user from the original request. Which key applies follows the [content-view model](../access-control/overview.md): `read` gates published documents, while `find`/`find_by_id`/`count` with `draft = true` or `trash = true` are gated by `draft` / `trash` (each falling back to `update` when unset); writes use `create`/`update`/`delete`.
- **Field-level access** — for `find`/`find_by_id`, fields the user can't read are stripped from results. For `create`/`update`, fields the user can't write are silently removed from the input data.
- **Constrained read access** — if a read access function returns a filter table instead of `true`, those filters are merged into the query (same as the gRPC/admin behavior).

```lua
-- Default: access control is enforced (only shows posts the user can see)
local result = crap.collections.posts.find({
    where = { status = "published" },
})

-- Bypass access control for internal/admin operations
local all = crap.collections.posts.find({
    override_access = true,
})
```

## crap.collections.count(collection, query?)

Count documents matching a query. Returns an integer count.

**Only available inside hooks with transaction context.**

```lua
local n = crap.collections.posts.count()
local published = crap.collections.posts.count({
    where = { status = "published" },
})
```

### Query Parameters

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `where` | table | `{}` | Field filters. Same syntax as `find`. |
| `locale` | string | `nil` | Locale code for localized fields. |
| `override_access` | boolean | `false` | Bypass access control checks. |
| `draft` | boolean | `false` | Include draft documents. |
| `trash` | boolean | `false` | Count only soft-deleted documents (collections with `soft_delete = true`). |
| `search` | string | `nil` | FTS5 full-text search query (same as `find`). |

## crap.collections.update_many(collection, query, data, opts?)

Update multiple documents matching a query. Returns `{ modified = N }`.

**Atomic, all-or-nothing:** the entire operation runs in a single transaction. Access is checked for every matched document first (if `override_access = false`), and if a write then fails partway through — a validation error, a hook error, a constraint violation — the whole operation rolls back, leaving nothing modified. The number of documents a single bulk op may match is capped by `[server] bulk_max_documents` (default `0` = unlimited); exceeding it errors and changes nothing.

Runs the full per-document lifecycle by default: `before_validate` → field validation → `before_change` → DB update → `after_change` — the same pipeline as single-document `update`. Set `hooks = false` in opts to skip hooks and validation for performance on large batch operations.

Only provided fields are written (partial update). Absent fields are left unchanged — including checkbox fields, which are **not** reset to `0` as they would be in a full single-document update.

**Only available inside hooks with transaction context.**

```lua
local result = crap.collections.posts.update_many({
    where = { status = "draft" },
}, {
    status = "published",
})
print(result.modified)  -- number of updated documents

-- Skip hooks and validation for performance
local result = crap.collections.posts.update_many({
    where = { status = "draft" },
}, {
    status = "published",
}, { hooks = false })
```

### Query Parameters (2nd argument)

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `where` | table | `{}` | Field filters to match documents. |

### Options (4th argument)

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `locale` | string | `nil` | Locale code for localized fields. |
| `override_access` | boolean | `false` | Bypass access control checks. |
| `draft` | boolean | `false` | Include draft documents. |
| `hooks` | boolean | `true` | Run per-document lifecycle hooks. Set to `false` to skip all hooks (`before_validate`, `before_change`, `after_change`) and field validation. |
| `events` | boolean | `false` | Emit a per-document live-update event for each modified document. Bulk ops are **quiet by default** to avoid flooding subscribers; set `true` to notify event-stream subscribers. |

### Data (3rd argument)

The `data` table contains fields to update on all matched documents (partial update).

## crap.collections.list_versions(collection, id, opts?)

List version snapshots for a document, newest first. Returns a table with `docs` (an array of version summaries) and `pagination` metadata matching the standard pagination shape. Only available on collections with `versions` enabled.

**Only available inside hooks with transaction context.**

```lua
-- List the 10 most recent versions for a document
local result = crap.collections.posts.list_versions("abc123", { limit = 10 })
for _, v in ipairs(result.docs) do
    print(v.version, v.status, v.created_at, v.latest)
end

-- Paginate
local page2 = crap.collections.posts.list_versions("abc123", { limit = 10, offset = 10 })

-- Internal listing that must ignore collection-level read access (e.g. a migration)
local all = crap.collections.posts.list_versions("abc123", { override_access = true })
```

### Options

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `limit` | integer | `nil` | Maximum number of versions to return. Nil = all remaining. |
| `offset` | integer | `0` | Number of versions to skip (offset pagination). |
| `override_access` | boolean | `false` | Bypass access control checks. Set to `true` in trusted internal code (jobs, migrations) to bypass collection-level access checks. This was previously hardcoded to `true` — it is now opt-in. |

### Version summary shape

Each entry in `result.docs` has:

| Field | Type | Description |
|-------|------|-------------|
| `id` | string | Version row ID (not the parent document ID). |
| `version` | integer | Monotonically-increasing version number, newest first. |
| `status` | string | Version status (e.g. `"published"`, `"draft"`). |
| `latest` | boolean | `true` for the newest version. |
| `created_at` | string? | ISO 8601 timestamp the snapshot was taken (nil if unset). |

## crap.collections.restore_version(collection, id, version_id, opts?)

Restore a previous version: copies the snapshot data back onto the parent document and writes a new version row. Returns the restored document. Only available on collections with `versions` enabled.

**Only available inside hooks with transaction context.**

```lua
-- Restore version "v2-abc" of document "post-1"
local doc = crap.collections.posts.restore_version("post-1", "v2-abc")
print(doc.title)

-- Internal restore bypassing per-user access (e.g. automated rollback job)
crap.collections.posts.restore_version("post-1", "v2-abc", { override_access = true })
```

### Options

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `override_access` | boolean | `false` | Bypass access control checks. Set to `true` in trusted internal code (jobs, migrations) to bypass collection-level access checks. This was previously hardcoded to `true` — it is now opt-in. |

## crap.collections.delete_many(collection, query, opts?)

Delete multiple documents matching a query. Returns `{ deleted = N, skipped = N }`. For upload collections, associated files are automatically cleaned up from disk for each deleted document. Documents that are still referenced by other documents are skipped (hard delete only) and reported in `skipped`.

**Atomic, all-or-nothing:** the entire operation runs in a single transaction. Access is checked for every matched document first (if `override_access = false`); a real failure partway through rolls everything back. Documents still referenced by others are **skipped** (counted in `skipped`), not treated as failures. The match size is capped by `[server] bulk_max_documents` (default `0` = unlimited); exceeding it errors and changes nothing.

Fires per-document lifecycle hooks (`before_delete`, `after_delete`) by default. Set `hooks = false` in opts to skip for performance on large batch operations.

**Only available inside hooks with transaction context.**

```lua
local result = crap.collections.posts.delete_many({
    where = { status = "archived" },
})
print(result.deleted)  -- number of deleted documents
print(result.skipped)  -- number skipped due to outstanding references

-- Bypass access control for internal operations
local result = crap.collections.posts.delete_many({
    where = { status = "archived" },
}, { override_access = true })

-- Skip hooks for performance
local result = crap.collections.posts.delete_many({
    where = { status = "archived" },
}, { hooks = false })
```

### Query Parameters (2nd argument)

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `where` | table | `{}` | Field filters to match documents. |

### Options (3rd argument)

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `override_access` | boolean | `false` | Bypass access control checks. |
| `hooks` | boolean | `true` | Run per-document lifecycle hooks. Set to `false` to skip `before_delete` and `after_delete` hooks. |
| `locale` | string | `nil` | Locale code for localized fields. |
| `force_hard_delete` | boolean | `false` | Skip `soft_delete` and remove rows permanently. |
| `events` | boolean | `false` | Emit a per-document live-update event for each deleted document. Bulk ops are **quiet by default**; set `true` to notify event-stream subscribers. |
