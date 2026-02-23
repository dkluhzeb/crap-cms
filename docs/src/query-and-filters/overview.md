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

## Field Validation

All filter field names and `order_by` fields are validated against the collection's field definitions. Invalid field names return an error. This prevents SQL injection via field names.

## Valid Filter Fields

You can filter on any column in the collection table:

- User-defined fields (that have parent columns)
- `id`
- `created_at` (if timestamps enabled)
- `updated_at` (if timestamps enabled)

You cannot filter on join-table fields (has-many relationships, arrays) directly.
