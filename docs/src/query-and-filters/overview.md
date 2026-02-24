# Query & Filters

Unified reference for querying documents across both the Lua API and gRPC API.

## Filter Operators

| Operator | Lua | gRPC (where) | SQL |
|----------|-----|-------------|-----|
| Equals | `status = "published"` or `{ equals = "val" }` | `{"equals": "val"}` | `field = ?` |
| Not equals | `{ not_equals = "val" }` | `{"not_equals": "val"}` | `field != ?` |
| Like | `{ like = "pattern%" }` | `{"like": "pattern%"}` | `field LIKE ?` |
| Contains | `{ contains = "text" }` | `{"contains": "text"}` | `field LIKE '%text%'` |
| Greater than | `{ greater_than = "10" }` | `{"greater_than": "10"}` | `field > ?` |
| Less than | `{ less_than = "10" }` | `{"less_than": "10"}` | `field < ?` |
| Greater/equal | `{ greater_than_or_equal = "10" }` | `{"greater_than_or_equal": "10"}` | `field >= ?` |
| Less/equal | `{ less_than_or_equal = "10" }` | `{"less_than_or_equal": "10"}` | `field <= ?` |
| In | `{ ["in"] = { "a", "b" } }` | `{"in": ["a", "b"]}` | `field IN (?, ?)` |
| Not in | `{ not_in = { "a", "b" } }` | `{"not_in": ["a", "b"]}` | `field NOT IN (?, ?)` |
| Exists | `{ exists = true }` | `{"exists": true}` | `field IS NOT NULL` |
| Not exists | `{ not_exists = true }` | `{"not_exists": true}` | `field IS NULL` |

## Sorting

Prefix a field name with `-` for descending order.

**Lua:**

```lua
crap.collections.find("posts", { order_by = "-created_at" })
```

**gRPC:**

```bash
grpcurl -plaintext -d '{
    "collection": "posts",
    "order_by": "-created_at"
}' localhost:50051 crap.ContentAPI/Find
```

## Pagination

Use `limit` and `offset` for pagination. The `total` field in the response gives the total count before pagination.

**Lua:**

```lua
local result = crap.collections.find("posts", {
    limit = 10,
    offset = 20,
})
-- result.total = 150 (total matching documents)
-- result.documents = 10 (this page)
```

**gRPC:**

```bash
grpcurl -plaintext -d '{
    "collection": "posts",
    "limit": "10",
    "offset": "20"
}' localhost:50051 crap.ContentAPI/Find
```

## Combining Filters

Multiple filters are combined with AND:

**Lua:**

```lua
crap.collections.find("posts", {
    filters = {
        status = "published",
        created_at = { greater_than = "2024-01-01" },
        title = { contains = "update" },
    },
    order_by = "-created_at",
    limit = 10,
})
```

**gRPC:**

```bash
grpcurl -plaintext -d '{
    "collection": "posts",
    "filters": { "status": "published" },
    "where": "{\"created_at\":{\"greater_than\":\"2024-01-01\"},\"title\":{\"contains\":\"update\"}}",
    "order_by": "-created_at",
    "limit": "10"
}' localhost:50051 crap.ContentAPI/Find
```

## OR Filters

Use the `or` key to combine groups of conditions with OR logic. Each element in the `or` array is an object whose fields are AND-ed together. Multiple `or` groups are joined with OR.

**Lua:**

```lua
-- title contains "hello" OR category = "news"
crap.collections.find("posts", {
    filters = {
        ["or"] = {
            { title = { contains = "hello" } },
            { category = "news" },
        },
    },
})

-- status = "published" AND (title contains "hello" OR title contains "world")
crap.collections.find("posts", {
    filters = {
        status = "published",
        ["or"] = {
            { title = { contains = "hello" } },
            { title = { contains = "world" } },
        },
    },
})

-- Multi-condition groups: (status = "published" AND title contains "hello") OR (status = "draft")
crap.collections.find("posts", {
    filters = {
        ["or"] = {
            { status = "published", title = { contains = "hello" } },
            { status = "draft" },
        },
    },
})
```

**gRPC:**

```bash
# title contains "hello" OR category = "news"
grpcurl -plaintext -d '{
    "collection": "posts",
    "where": "{\"or\":[{\"title\":{\"contains\":\"hello\"}},{\"category\":\"news\"}]}"
}' localhost:50051 crap.ContentAPI/Find

# status = "published" AND (title contains "hello" OR title contains "world")
grpcurl -plaintext -d '{
    "collection": "posts",
    "where": "{\"status\":\"published\",\"or\":[{\"title\":{\"contains\":\"hello\"}},{\"title\":{\"contains\":\"world\"}}]}"
}' localhost:50051 crap.ContentAPI/Find
```

Top-level filters and `or` groups are combined with AND. Each object inside the `or` array can have multiple fields which are AND-ed together within that group.

## Field Selection (`select`)

Use `select` to specify which fields to return. Reduces data transfer and skips relationship hydration/population for non-selected fields. The `id`, `created_at`, and `updated_at` fields are always included.

**Lua:**

```lua
-- Return only title and status
crap.collections.find("posts", {
    select = { "title", "status" },
})

-- Works with find_by_id too
crap.collections.find_by_id("posts", id, {
    select = { "title", "status" },
})
```

**gRPC:**

```bash
# Return only title and status fields
grpcurl -plaintext -d '{
    "collection": "posts",
    "select": ["title", "status"]
}' localhost:50051 crap.ContentAPI/Find

# FindByID with select
grpcurl -plaintext -d '{
    "collection": "posts",
    "id": "abc123",
    "select": ["title", "status"]
}' localhost:50051 crap.ContentAPI/FindByID
```

**Behavior:**
- `select` is optional. When omitted or empty, all fields are returned (backward compatible).
- System fields (`id`, `created_at`, `updated_at`) are always included.
- Selecting a group field name (e.g., `"seo"`) includes all its sub-fields.
- Relationship fields not in `select` are skipped during population (saves N+1 queries).

## Field Validation

All filter field names and `order_by` fields are validated against the collection's field definitions. Invalid field names return an error. This prevents SQL injection via field names.

## Draft Parameter (Versioned Collections)

Collections with `versions = { drafts = true }` automatically filter by `_status = 'published'` on `Find` queries. Use the `draft` parameter to change this behavior.

**Lua:**

```lua
-- Default: only published documents
local published = crap.collections.find("articles", {})

-- Include drafts
local all = crap.collections.find("articles", { draft = true })

-- FindByID: get the latest version (may be a draft)
local latest = crap.collections.find_by_id("articles", id, { draft = true })
```

**gRPC:**

```bash
# Default: only published
grpcurl -plaintext -d '{"collection": "articles"}' \
    localhost:50051 crap.ContentAPI/Find

# Include drafts
grpcurl -plaintext -d '{"collection": "articles", "draft": true}' \
    localhost:50051 crap.ContentAPI/Find

# FindByID: get latest version snapshot
grpcurl -plaintext -d '{"collection": "articles", "id": "abc123", "draft": true}' \
    localhost:50051 crap.ContentAPI/FindByID
```

You can also filter directly on `_status`:

```lua
crap.collections.find("articles", {
    filters = { _status = "draft" },
})
```

See [Versions & Drafts](../collections/versions.md) for the full workflow.

## Valid Filter Fields

You can filter on any column in the collection table:

- User-defined fields (that have parent columns)
- `id`
- `created_at` (if timestamps enabled)
- `updated_at` (if timestamps enabled)
- `_status` (if `versions.drafts` enabled)

You cannot filter on join-table fields (has-many relationships, arrays) directly.
