# Where Clause

The `Find` RPC supports an advanced `where` parameter for operator-based filtering. This is a JSON string containing field filters with operators.

## Format

The `where` parameter is a JSON-encoded string:

```json
{
    "field_name": { "operator": "value" },
    "field_name2": { "operator": "value2" }
}
```

Multiple fields are combined with AND.

## Operators

| Operator | JSON Syntax | SQL |
|----------|-----------|-----|
| `equals` | `{"field": {"equals": "value"}}` | `field = ?` |
| `not_equals` | `{"field": {"not_equals": "value"}}` | `field != ?` |
| `like` | `{"field": {"like": "pattern%"}}` | `field LIKE ?` |
| `contains` | `{"field": {"contains": "text"}}` | `field LIKE '%text%' ESCAPE '\'` (wildcards escaped) |
| `greater_than` | `{"field": {"greater_than": "10"}}` | `field > ?` |
| `less_than` | `{"field": {"less_than": "10"}}` | `field < ?` |
| `greater_than_or_equal` | `{"field": {"greater_than_or_equal": "10"}}` | `field >= ?` |
| `less_than_or_equal` | `{"field": {"less_than_or_equal": "10"}}` | `field <= ?` |
| `in` | `{"field": {"in": ["a", "b"]}}` | `field IN (?, ?)` |
| `not_in` | `{"field": {"not_in": ["a", "b"]}}` | `field NOT IN (?, ?)` |
| `exists` | `{"field": {"exists": true}}` | `field IS NOT NULL` |
| `not_exists` | `{"field": {"not_exists": true}}` | `field IS NULL` |

> **Note:** For `exists`/`not_exists`, the value is ignored — only the key matters. Field values must be strings or operator objects — numeric/boolean shorthand (e.g., `{"count": 42}`) is not supported in the gRPC JSON `where` clause (use `{"count": {"equals": "42"}}` instead).

## Examples

```bash
# Published posts with "hello" in the title
grpcurl -plaintext -d '{
    "collection": "posts",
    "where": "{\"status\":{\"equals\":\"published\"},\"title\":{\"contains\":\"hello\"}}"
}' localhost:50051 crap.ContentAPI/Find

# Posts with status in a list
grpcurl -plaintext -d '{
    "collection": "posts",
    "where": "{\"status\":{\"in\":[\"draft\",\"published\"]}}"
}' localhost:50051 crap.ContentAPI/Find

# Posts created after a date
grpcurl -plaintext -d '{
    "collection": "posts",
    "where": "{\"created_at\":{\"greater_than\":\"2024-01-01\"}}"
}' localhost:50051 crap.ContentAPI/Find

# Posts with null status
grpcurl -plaintext -d '{
    "collection": "posts",
    "where": "{\"status\":{\"not_exists\":true}}"
}' localhost:50051 crap.ContentAPI/Find
```

## Nested Field Filters (Dot Notation)

Filter on sub-fields of group, array, blocks, and has-many relationship fields using dot notation:

```bash
# Group sub-field: seo.meta_title → seo__meta_title column
grpcurl -plaintext -d '{
    "collection": "pages",
    "where": "{\"seo.meta_title\":{\"contains\":\"SEO\"}}"
}' localhost:50051 crap.ContentAPI/Find

# Array sub-field: products with any variant color "red"
grpcurl -plaintext -d '{
    "collection": "products",
    "where": "{\"variants.color\":{\"equals\":\"red\"}}"
}' localhost:50051 crap.ContentAPI/Find

# Block sub-field: posts with content containing "hello"
grpcurl -plaintext -d '{
    "collection": "posts",
    "where": "{\"content.body\":{\"contains\":\"hello\"}}"
}' localhost:50051 crap.ContentAPI/Find

# Block type filter
grpcurl -plaintext -d '{
    "collection": "posts",
    "where": "{\"content._block_type\":{\"equals\":\"image\"}}"
}' localhost:50051 crap.ContentAPI/Find

# Has-many relationship: posts with a specific tag
grpcurl -plaintext -d '{
    "collection": "posts",
    "where": "{\"tags.id\":{\"equals\":\"tag-123\"}}"
}' localhost:50051 crap.ContentAPI/Find
```

Array and block filters use `EXISTS` subqueries — they match parent documents where **at least one** row matches. All filter operators work with dot notation paths.

See [Query & Filters](../query-and-filters/overview.md#nested-field-filters-dot-notation) for the full reference.

## OR Filters

Use the `or` key to combine groups of conditions with OR logic. Each element in the `or` array is an object whose fields are AND-ed together. Top-level filters outside `or` are AND-ed with the OR result.

```bash
# title contains "hello" OR category = "news"
grpcurl -plaintext -d '{
    "collection": "posts",
    "where": "{\"or\":[{\"title\":{\"contains\":\"hello\"}},{\"category\":{\"equals\":\"news\"}}]}"
}' localhost:50051 crap.ContentAPI/Find

# status = "published" AND (title contains "hello" OR title contains "world")
grpcurl -plaintext -d '{
    "collection": "posts",
    "where": "{\"status\":{\"equals\":\"published\"},\"or\":[{\"title\":{\"contains\":\"hello\"}},{\"title\":{\"contains\":\"world\"}}]}"
}' localhost:50051 crap.ContentAPI/Find

# Multi-condition groups: (status = "published" AND title contains "hello") OR (status = "draft")
grpcurl -plaintext -d '{
    "collection": "posts",
    "where": "{\"or\":[{\"status\":{\"equals\":\"published\"},\"title\":{\"contains\":\"hello\"}},{\"status\":{\"equals\":\"draft\"}}]}"
}' localhost:50051 crap.ContentAPI/Find
```

