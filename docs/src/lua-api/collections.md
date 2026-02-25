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

Find documents matching a query. Returns a result table with `documents` and `total`.

**Only available inside hooks with transaction context.**

```lua
local result = crap.collections.find("posts", {
    filters = {
        status = "published",
        title = { contains = "hello" },
    },
    order_by = "-created_at",
    limit = 10,
    offset = 0,
    depth = 1,
})

-- result.documents = array of document tables
-- result.total = total count (before limit/offset)

for _, doc in ipairs(result.documents) do
    print(doc.id, doc.title)
end
```

### Query Parameters

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `filters` | table | `{}` | Field filters. See [Filter Operators](filter-operators.md). Supports `["or"]` key for OR groups. |
| `order_by` | string | `nil` | Sort field. Prefix with `-` for descending. |
| `limit` | integer | `nil` | Max results to return. |
| `offset` | integer | `nil` | Number of results to skip. |
| `depth` | integer | `0` | Population depth for relationship fields. |
| `select` | string[] | `nil` | Fields to return. `nil` = all fields. Always includes `id`, `created_at`, `updated_at`. |
| `overrideAccess` | boolean | `true` | Skip access control checks. Set to `false` to enforce collection-level and field-level access for the current user. |

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
| `overrideAccess` | boolean | `true` | Skip access control checks. Set to `false` to enforce collection-level and field-level access for the current user. |

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

## Access Control in Hooks

By default, all Lua CRUD functions bypass access control (`overrideAccess = true`). This matches PayloadCMS behavior — hooks are trusted server-side code with full access.

When you set `overrideAccess = false`, the function enforces the same access rules as the external API:

- **Collection-level access** — the relevant access function (`read`, `create`, `update`, `delete`) is called with the authenticated user from the original request.
- **Field-level access** — for `find`/`find_by_id`, fields the user can't read are stripped from results. For `create`/`update`, fields the user can't write are silently removed from the input data.
- **Constrained read access** — if a read access function returns a filter table instead of `true`, those filters are merged into the query (same as the gRPC/admin behavior).

```lua
-- Example: fetch only posts the current user is allowed to see
local result = crap.collections.find("posts", {
    filters = { status = "published" },
    overrideAccess = false,
})
```

## crap.collections.count(collection, query?)

Count documents matching a query. Returns an integer count.

**Only available inside hooks with transaction context.**

```lua
local n = crap.collections.count("posts")
local published = crap.collections.count("posts", {
    filters = { status = "published" },
})
```

### Query Parameters

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `filters` | table | `{}` | Field filters. Same syntax as `find`. |
| `locale` | string | `nil` | Locale code for localized fields. |
| `overrideAccess` | boolean | `true` | Skip access control checks. |
| `draft` | boolean | `false` | Include draft documents. |

## crap.collections.update_many(collection, query, data, opts?)

Update multiple documents matching a query. Returns `{ modified = N }`.

**All-or-nothing semantics:** finds all matching documents, checks update access for each (if `overrideAccess = false`), and only proceeds if all pass. If any document fails access, an error is returned and nothing is modified.

Does **not** fire per-document hooks (before_change, after_change, etc.).

**Only available inside hooks with transaction context.**

```lua
local result = crap.collections.update_many("posts", {
    filters = { status = "draft" },
}, {
    status = "published",
})
print(result.modified)  -- number of updated documents
```

### Query Parameters

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `filters` | table | `{}` | Field filters to match documents. |
| `locale` | string | `nil` | Locale code for localized fields. |
| `overrideAccess` | boolean | `true` | Skip access control checks. |
| `draft` | boolean | `false` | Include draft documents. |

### Data

The `data` table contains fields to update on all matched documents (partial update).

## crap.collections.delete_many(collection, query)

Delete multiple documents matching a query. Returns `{ deleted = N }`.

**All-or-nothing semantics:** finds all matching documents, checks delete access for each (if `overrideAccess = false`), and only proceeds if all pass.

Does **not** fire per-document hooks (before_delete, after_delete, etc.).

**Only available inside hooks with transaction context.**

```lua
local result = crap.collections.delete_many("posts", {
    filters = { status = "archived" },
})
print(result.deleted)  -- number of deleted documents

-- With access control enforcement
local result = crap.collections.delete_many("posts", {
    filters = { status = "archived" },
    overrideAccess = false,
})
```

### Query Parameters

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `filters` | table | `{}` | Field filters to match documents. |
| `overrideAccess` | boolean | `true` | Skip access control checks. |
