# crap.collections

Collection definition and runtime CRUD operations.

## crap.collections.define(slug, config)

Define a new collection. Call this in collection definition files (`collections/*.lua`).

```lua
crap.collections.define("posts", {
    labels = { singular = "Post", plural = "Posts" },
    fields = {
        { name = "title", type = "text", required = true },
    },
})
```

See [Collection Definition Schema](../collections/definition-schema.md) for all config options.

## crap.collections.config.get(slug)

Get a collection's current definition as a Lua table. The returned table is round-trip
compatible with `define()` — you can modify it and pass it back.

Returns `nil` if the collection doesn't exist.

```lua
local def = crap.collections.config.get("posts")
if def then
    -- Add a field
    def.fields[#def.fields + 1] = { name = "extra", type = "text" }
    crap.collections.define("posts", def)
end
```

## crap.collections.config.list()

Get all registered collections as a slug-keyed table. Iterate with `pairs()`.

```lua
for slug, def in pairs(crap.collections.config.list()) do
    if def.upload then
        -- Add alt_text to every upload collection
        def.fields[#def.fields + 1] = { name = "alt_text", type = "text" }
        crap.collections.define(slug, def)
    end
end
```

See [Plugins](../plugins/overview.md) for patterns using these functions.

## crap.collections.find(collection, query?)

Find documents matching a query. Returns a result table with `documents` and `pagination`.

**Only available inside hooks with transaction context.**

```lua
local result = crap.collections.find("posts", {
    where = {
        status = "published",
        title = { contains = "hello" },
    },
    order_by = "-created_at",
    limit = 10,
    page = 1,
    depth = 1,
})

-- result.documents               = array of document tables
-- result.pagination.totalDocs    = total count (before limit/page)
-- result.pagination.limit        = applied limit
-- result.pagination.totalPages   = total pages (offset mode only)
-- result.pagination.page         = current page (offset mode only, 1-based)
-- result.pagination.pageStart    = 1-based index of first doc on this page
-- result.pagination.hasNextPage  = boolean
-- result.pagination.hasPrevPage  = boolean
-- result.pagination.prevPage     = previous page number (nil if first page)
-- result.pagination.nextPage     = next page number (nil if last page)
-- result.pagination.startCursor  = opaque cursor of first doc (cursor mode only)
-- result.pagination.endCursor    = opaque cursor of last doc (cursor mode only)

for _, doc in ipairs(result.documents) do
    print(doc.id, doc.title)
end
```

### Query Parameters

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `where` | table | `{}` | Field filters. See [Filter Operators](filter-operators.md). Supports `["or"]` key for OR groups. |
| `order_by` | string | `nil` | Sort field. Prefix with `-` for descending. |
| `limit` | integer | `nil` | Max results to return. |
| `page` | integer | `1` | Page number (1-based). Converted to offset internally. |
| `offset` | integer | `nil` | Number of results to skip (backward compat alias for `page`). |
| `after_cursor` | string | `nil` | Forward cursor from a previous `result.pagination.endCursor`. Fetches the page after the cursor position. Mutually exclusive with `page`/`offset`/`before_cursor`. Only effective when `[pagination] mode = "cursor"` in `crap.toml`. |
| `before_cursor` | string | `nil` | Backward cursor from a previous `result.pagination.startCursor`. Fetches the page before the cursor position. Mutually exclusive with `page`/`offset`/`after_cursor`. Only effective when `[pagination] mode = "cursor"` in `crap.toml`. |
| `depth` | integer | `0` | Population depth for relationship fields. |
| `select` | string[] | `nil` | Fields to return. `nil` = all fields. Always includes `id`. When specified, `created_at` and `updated_at` are only included if explicitly listed. |
| `draft` | boolean | `false` | Include draft documents. Only affects versioned collections with `drafts = true`. |
| `locale` | string | `nil` | Locale code for localized fields (e.g., `"en"`, `"de"`). |
| `overrideAccess` | boolean | `true` | Skip access control checks. Set to `false` to enforce collection-level and field-level access for the current user. |
| `search` | string | `nil` | FTS5 full-text search query. Filters results to documents matching this search term. |

## crap.collections.find_by_id(collection, id, opts?)

Find a single document by ID. Returns the document table or `nil`.

**Only available inside hooks with transaction context.**

```lua
local doc = crap.collections.find_by_id("posts", "abc123")
if doc then
    print(doc.title)
end

-- With population depth
local doc = crap.collections.find_by_id("posts", "abc123", { depth = 2 })

-- With field selection (only return title and status)
local doc = crap.collections.find_by_id("posts", "abc123", { select = { "title", "status" } })
```

### Options

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `depth` | integer | `0` | Population depth for relationship fields. |
| `select` | string[] | `nil` | Fields to return. `nil` = all fields. Always includes `id`. |
| `draft` | boolean | `false` | Return the latest draft version snapshot instead of the published main-table data. Only affects versioned collections with `drafts = true`. |
| `locale` | string | `nil` | Locale code for localized fields (e.g., `"en"`, `"de"`). |
| `overrideAccess` | boolean | `true` | Skip access control checks. Set to `false` to enforce collection-level and field-level access for the current user. |

## crap.collections.create(collection, data, opts?)

Create a new document. Returns the created document.

**Only available inside hooks with transaction context.**

```lua
local doc = crap.collections.create("posts", {
    title = "New Post",
    slug = "new-post",
})
print(doc.id)  -- auto-generated nanoid

-- Create as draft (versioned collections only)
local draft = crap.collections.create("articles", {
    title = "Work in progress",
}, { draft = true })
```

### Options

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `locale` | string | `nil` | Locale code for localized fields. |
| `draft` | boolean | `false` | Create as draft. Skips required field validation. Only affects versioned collections with `drafts = true`. |
| `overrideAccess` | boolean | `true` | Skip access control checks. Set to `false` to enforce collection-level and field-level access for the current user. |
| `hooks` | boolean | `true` | Run lifecycle hooks. Set to `false` to skip all hooks (before_validate, before_change, after_change) and validation. The DB operation still executes. |

## crap.collections.update(collection, id, data, opts?)

Update an existing document. Returns the updated document.

**Only available inside hooks with transaction context.**

```lua
local doc = crap.collections.update("posts", "abc123", {
    title = "Updated Title",
})

-- Draft update: saves a version snapshot only, main table unchanged
crap.collections.update("articles", "abc123", {
    title = "Still editing...",
}, { draft = true })
```

### Options

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `locale` | string | `nil` | Locale code for localized fields. |
| `draft` | boolean | `false` | Version-only save. Creates a draft version snapshot without modifying the main table. Only affects versioned collections with `drafts = true`. |
| `unpublish` | boolean | `false` | Set document status to draft and create a draft version snapshot. Ignores the `data` fields when unpublishing. Only affects versioned collections. |
| `overrideAccess` | boolean | `true` | Skip access control checks. Set to `false` to enforce collection-level and field-level access for the current user. |
| `hooks` | boolean | `true` | Run lifecycle hooks. Set to `false` to skip all hooks (before_validate, before_change, after_change) and validation. The DB operation still executes. |

### Auth Collections

For collections with `auth = true`, the `password` field is automatically handled:
- On **create**, if the data contains a `password` key, it is extracted before hooks run, hashed with Argon2id, and stored in the hidden `_password_hash` column. Hooks never see the raw password.
- On **update**, same pattern — if `password` is present and non-empty, the password is updated. Leave it out or set it to `""` to keep the current password.

This matches the behavior of the gRPC API and admin UI.

## crap.collections.delete(collection, id, opts?)

Delete a document. Returns `true` on success.

**Only available inside hooks with transaction context.**

```lua
crap.collections.delete("posts", "abc123")

-- With access control enforcement
crap.collections.delete("posts", "abc123", { overrideAccess = false })
```

### Options

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `overrideAccess` | boolean | `true` | Skip access control checks. Set to `false` to enforce collection-level access for the current user. |
| `hooks` | boolean | `true` | Run lifecycle hooks. Set to `false` to skip before_delete and after_delete hooks. The DB operation still executes. |

## Lifecycle Hooks in Lua CRUD

Lua CRUD operations run the **same lifecycle hooks** as the gRPC API and admin UI:

- **`create`**: before_validate → validate → before_change → DB insert → after_change
- **`update`**: before_validate → validate → before_change → DB update → after_change
- **`delete`**: before_delete → DB delete → after_delete
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
    crap.collections.create("audit", { action = ctx.operation })
    return ctx
end
```

### Skipping Hooks

Pass `hooks = false` to any write CRUD call to skip all lifecycle hooks:

```lua
-- Create without triggering any hooks
crap.collections.create("logs", { message = "raw insert" }, { hooks = false })
```

## Access Control in Hooks

By default, all Lua CRUD functions bypass access control (`overrideAccess = true`). Hooks are trusted server-side code with full access.

When you set `overrideAccess = false`, the function enforces the same access rules as the external API:

- **Collection-level access** — the relevant access function (`read`, `create`, `update`, `delete`) is called with the authenticated user from the original request.
- **Field-level access** — for `find`/`find_by_id`, fields the user can't read are stripped from results. For `create`/`update`, fields the user can't write are silently removed from the input data.
- **Constrained read access** — if a read access function returns a filter table instead of `true`, those filters are merged into the query (same as the gRPC/admin behavior).

```lua
-- Example: fetch only posts the current user is allowed to see
local result = crap.collections.find("posts", {
    where = { status = "published" },
    overrideAccess = false,
})
```

## crap.collections.count(collection, query?)

Count documents matching a query. Returns an integer count.

**Only available inside hooks with transaction context.**

```lua
local n = crap.collections.count("posts")
local published = crap.collections.count("posts", {
    where = { status = "published" },
})
```

### Query Parameters

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `where` | table | `{}` | Field filters. Same syntax as `find`. |
| `locale` | string | `nil` | Locale code for localized fields. |
| `overrideAccess` | boolean | `true` | Skip access control checks. |
| `draft` | boolean | `false` | Include draft documents. |
| `search` | string | `nil` | FTS5 full-text search query (same as `find`). |

## crap.collections.update_many(collection, query, data, opts?)

Update multiple documents matching a query. Returns `{ modified = N }`.

**All-or-nothing semantics:** finds all matching documents, checks update access for each (if `overrideAccess = false`), and only proceeds if all pass. If any document fails access, an error is returned and nothing is modified.

Fires per-document lifecycle hooks (`before_change`, `after_change`) by default. Set `hooks = false` in opts to skip for performance on large batch operations.

**Only available inside hooks with transaction context.**

```lua
local result = crap.collections.update_many("posts", {
    where = { status = "draft" },
}, {
    status = "published",
})
print(result.modified)  -- number of updated documents

-- Skip hooks for performance
local result = crap.collections.update_many("posts", {
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
| `overrideAccess` | boolean | `true` | Skip access control checks. |
| `draft` | boolean | `false` | Include draft documents. |
| `hooks` | boolean | `true` | Run per-document lifecycle hooks. Set to `false` to skip `before_change` and `after_change` hooks. |

### Data (3rd argument)

The `data` table contains fields to update on all matched documents (partial update).

## crap.collections.delete_many(collection, query, opts?)

Delete multiple documents matching a query. Returns `{ deleted = N }`.

**All-or-nothing semantics:** finds all matching documents, checks delete access for each (if `overrideAccess = false`), and only proceeds if all pass.

Fires per-document lifecycle hooks (`before_delete`, `after_delete`) by default. Set `hooks = false` in opts to skip for performance on large batch operations.

**Only available inside hooks with transaction context.**

```lua
local result = crap.collections.delete_many("posts", {
    where = { status = "archived" },
})
print(result.deleted)  -- number of deleted documents

-- With access control enforcement
local result = crap.collections.delete_many("posts", {
    where = { status = "archived" },
}, { overrideAccess = false })

-- Skip hooks for performance
local result = crap.collections.delete_many("posts", {
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
| `overrideAccess` | boolean | `true` | Skip access control checks. |
| `hooks` | boolean | `true` | Run per-document lifecycle hooks. Set to `false` to skip `before_delete` and `after_delete` hooks. |
| `locale` | string | `nil` | Locale code for localized fields. |
| `draft` | boolean | `false` | Include draft documents. |
