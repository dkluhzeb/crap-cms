# Query & Filters

Unified reference for querying documents across both the Lua API and gRPC API.

## Filter Operators

| Operator | Lua | gRPC (where) | SQL |
|----------|-----|-------------|-----|
| Equals | `status = "published"` or `{ equals = "val" }` | `{"equals": "val"}` | `field = ?` |
| Not equals | `{ not_equals = "val" }` | `{"not_equals": "val"}` | `field != ?` |
| Like | `{ like = "pattern%" }` | `{"like": "pattern%"}` | `field LIKE ?` |
| Contains | `{ contains = "text" }` | `{"contains": "text"}` | `field LIKE '%text%' ESCAPE '\'` (wildcards `%` and `_` in the search text are escaped) |
| Greater than | `{ greater_than = "10" }` | `{"greater_than": "10"}` | `field > ?` |
| Less than | `{ less_than = "10" }` | `{"less_than": "10"}` | `field < ?` |
| Greater/equal | `{ greater_than_or_equal = "10" }` | `{"greater_than_or_equal": "10"}` | `field >= ?` |
| Less/equal | `{ less_than_or_equal = "10" }` | `{"less_than_or_equal": "10"}` | `field <= ?` |
| In | `{ ["in"] = { "a", "b" } }` | `{"in": ["a", "b"]}` | `field IN (?, ?)` |
| Not in | `{ not_in = { "a", "b" } }` | `{"not_in": ["a", "b"]}` | `field NOT IN (?, ?)` |
| Exists | `{ exists = true }` | `{"exists": true}` | `field IS NOT NULL` |
| Not exists | `{ not_exists = true }` | `{"not_exists": true}` | `field IS NULL` |

> **Note:** For `exists`/`not_exists`, the value is ignored — only the key matters. Use `not_exists` for IS NULL (not `{ exists = false }`).
>
> **gRPC shorthand limitation:** In Lua, bare values like `{ count = 42 }` or `{ active = true }` are coerced to string equals. The gRPC `where` JSON only accepts string or operator object values — numeric/boolean shorthand is not supported.

## Sorting

Prefix a field name with `-` for descending order. When `order_by` is omitted, results are sorted by `created_at DESC` (newest first) for collections with timestamps, or `id ASC` otherwise. When sorting by a non-id field, an `id` tiebreaker is always appended for stable ordering.

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

Use `limit` and `page` for pagination. The response includes a nested `pagination` object with total count and page info.

**Lua:**

```lua
local result = crap.collections.find("posts", {
    limit = 10,
    page = 3,
})
-- result.pagination.totalDocs   = 150 (total matching documents)
-- result.pagination.limit       = 10
-- result.pagination.totalPages  = 15
-- result.pagination.page        = 3   (1-based)
-- result.pagination.pageStart   = 21  (1-based index of first doc on this page)
-- result.pagination.hasNextPage = true
-- result.pagination.hasPrevPage = true
-- result.pagination.prevPage    = 2
-- result.pagination.nextPage    = 4
-- #result.documents             = 10  (this page)
```

**gRPC:**

```bash
grpcurl -plaintext -d '{
    "collection": "posts",
    "limit": "10",
    "page": "3"
}' localhost:50051 crap.ContentAPI/Find
```

## Cursor-Based Pagination

Cursor-based pagination is opt-in via `[pagination] mode = "cursor"` in `crap.toml`. When enabled, the `pagination` object includes opaque `startCursor` and `endCursor` tokens instead of `page`/`totalPages`. These represent the cursors of the first and last documents in the result set. Pass `after_cursor` (forward) or `before_cursor` (backward) on the next request to navigate from any cursor position.

`after_cursor`/`before_cursor` and `page` are mutually exclusive. `after_cursor` and `before_cursor` are also mutually exclusive with each other.

**Lua:**

```lua
-- First page
local result = crap.collections.find("posts", {
    order_by = "-created_at",
    limit = 10,
})
-- result.pagination.hasNextPage  = true
-- result.pagination.hasPrevPage  = false
-- result.pagination.startCursor  = "eyJpZCI6ImFiYzEyMyJ9"  (cursor of first doc)
-- result.pagination.endCursor    = "eyJpZCI6Inh5ejc4OSJ9"  (cursor of last doc)

-- Next page (forward)
local page2 = crap.collections.find("posts", {
    order_by = "-created_at",
    limit = 10,
    after_cursor = result.pagination.endCursor,
})

-- Previous page (backward)
local page1_again = crap.collections.find("posts", {
    order_by = "-created_at",
    limit = 10,
    before_cursor = page2.pagination.startCursor,
})
```

**gRPC:**

```bash
# First page
grpcurl -plaintext -d '{
    "collection": "posts",
    "order_by": "-created_at",
    "limit": "10"
}' localhost:50051 crap.ContentAPI/Find
# Response pagination includes start_cursor / end_cursor when cursor mode is active

# Next page (forward)
grpcurl -plaintext -d '{
    "collection": "posts",
    "order_by": "-created_at",
    "limit": "10",
    "after_cursor": "eyJpZCI6Inh5ejc4OSJ9"
}' localhost:50051 crap.ContentAPI/Find

# Previous page (backward)
grpcurl -plaintext -d '{
    "collection": "posts",
    "order_by": "-created_at",
    "limit": "10",
    "before_cursor": "eyJpZCI6ImFiYzEyMyJ9"
}' localhost:50051 crap.ContentAPI/Find
```

Cursors encode the position of a document in the sorted result set. They are opaque — do not parse or construct them manually. `startCursor` and `endCursor` are always present when the result set is non-empty.

## Combining Filters

Multiple filters are combined with AND:

**Lua:**

```lua
crap.collections.find("posts", {
    where = {
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
    "where": "{\"status\":\"published\",\"created_at\":{\"greater_than\":\"2024-01-01\"},\"title\":{\"contains\":\"update\"}}",
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
    where = {
        ["or"] = {
            { title = { contains = "hello" } },
            { category = "news" },
        },
    },
})

-- status = "published" AND (title contains "hello" OR title contains "world")
crap.collections.find("posts", {
    where = {
        status = "published",
        ["or"] = {
            { title = { contains = "hello" } },
            { title = { contains = "world" } },
        },
    },
})

-- Multi-condition groups: (status = "published" AND title contains "hello") OR (status = "draft")
crap.collections.find("posts", {
    where = {
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
    where = { _status = "draft" },
})
```

See [Versions & Drafts](../collections/versions.md) for the full workflow.

## Nested Field Filters (Dot Notation)

You can filter on sub-fields of group, array, blocks, and has-many relationship fields using dot notation.

### Group Fields

Group sub-fields can be filtered using dot notation. Internally, `seo.meta_title` is converted to `seo__meta_title` (the flat column name). The double-underscore syntax also continues to work.

**Lua:**

```lua
crap.collections.find("pages", {
    where = {
        ["seo.meta_title"] = { contains = "SEO" },
    },
})
```

**gRPC:**

```bash
grpcurl -plaintext -d '{
    "collection": "pages",
    "where": "{\"seo.meta_title\":{\"contains\":\"SEO\"}}"
}' localhost:50051 crap.ContentAPI/Find
```

### Array Sub-Fields

Filter by sub-field values in array rows. Uses an `EXISTS` subquery against the array join table. Returns parent documents that have **at least one** array row matching the condition.

**Lua:**

```lua
-- Find products where any variant has color "red"
crap.collections.find("products", {
    where = {
        ["variants.color"] = "red",
    },
})

-- Group-in-array: filter by a group sub-field within array rows
-- (uses json_extract on the JSON column in the join table)
crap.collections.find("products", {
    where = {
        ["variants.dimensions.width"] = "10",
    },
})
```

### Block Sub-Fields

Filter by field values inside block rows. Uses `json_extract` on the block `data` column. Returns parent documents that have **at least one** block row matching.

**Lua:**

```lua
-- Find posts where any content block has body containing "hello"
crap.collections.find("posts", {
    where = {
        ["content.body"] = { contains = "hello" },
    },
})

-- Filter by block type
crap.collections.find("posts", {
    where = {
        ["content._block_type"] = "image",
    },
})

-- Group-in-block: filter by a group sub-field within block data
crap.collections.find("posts", {
    where = {
        ["content.meta.author"] = "Alice",
    },
})
```

### Has-Many Relationships

Filter by related document IDs. Uses an `EXISTS` subquery against the relationship join table.

**Lua:**

```lua
-- Find posts that have tag "tag-123"
crap.collections.find("posts", {
    where = {
        ["tags.id"] = "tag-123",
    },
})
```

### Combining Nested and Regular Filters

Nested field filters can be freely combined with regular column filters and OR groups:

**Lua:**

```lua
crap.collections.find("products", {
    where = {
        status = "published",
        ["variants.color"] = "red",
        ["or"] = {
            { ["content._block_type"] = "image" },
            { ["tags.id"] = "tag-featured" },
        },
    },
})
```

All filter operators (equals, contains, like, in, greater_than, etc.) work with nested field filters.

## Full-Text Search

Use the `search` parameter for fast full-text search powered by SQLite FTS5. This searches across all text-like fields (text, textarea, richtext, email, code) or the fields specified in `list_searchable_fields` in the collection's admin config.

**Lua:**

```lua
local result = crap.collections.find("posts", {
    search = "hello world",
    limit = 10,
})
```

**gRPC:**

```bash
grpcurl -plaintext -d '{
    "collection": "posts",
    "search": "hello world",
    "limit": "10"
}' localhost:50051 crap.ContentAPI/Find
```

**Behavior:**
- Each whitespace-separated word is treated as a literal search term (implicit AND).
- Results are ranked by relevance (FTS5 `rank`).
- `search` can be combined with `where` filters, pagination, sorting, and all other query parameters.
- Collections without text fields silently ignore the `search` parameter.
- The `search` parameter also works with `Count` to get the total number of matching documents.

**Indexed fields** are determined by:
1. `admin.list_searchable_fields` if configured on the collection.
2. Otherwise, all parent-level fields with types: text, textarea, richtext, email, code.

The FTS index is automatically created and rebuilt on server startup for every collection with text fields.

## Valid Filter Fields

You can filter on any column in the collection table:

- User-defined fields (that have parent columns)
- `id`
- `created_at` (if timestamps enabled)
- `updated_at` (if timestamps enabled)
- `_status` (if `versions.drafts` enabled)

Additionally, you can filter on sub-fields using dot notation:

- **Group sub-fields:** `group_name.sub_field` (syntactic sugar for `group_name__sub_field`)
- **Array sub-fields:** `array_name.sub_field` or `array_name.group.sub_field` (group-in-array)
- **Block sub-fields:** `blocks_name.field`, `blocks_name._block_type`, or `blocks_name.group.sub_field`
- **Has-many relationships:** `relationship_name.id`
