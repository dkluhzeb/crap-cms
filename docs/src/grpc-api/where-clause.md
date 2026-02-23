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
| `contains` | `{"field": {"contains": "text"}}` | `field LIKE '%text%'` |
| `greater_than` | `{"field": {"greater_than": "10"}}` | `field > ?` |
| `less_than` | `{"field": {"less_than": "10"}}` | `field < ?` |
| `greater_than_or_equal` | `{"field": {"greater_than_or_equal": "10"}}` | `field >= ?` |
| `less_than_or_equal` | `{"field": {"less_than_or_equal": "10"}}` | `field <= ?` |
| `in` | `{"field": {"in": ["a", "b"]}}` | `field IN (?, ?)` |
| `not_in` | `{"field": {"not_in": ["a", "b"]}}` | `field NOT IN (?, ?)` |
| `exists` | `{"field": {"exists": true}}` | `field IS NOT NULL` |
| `not_exists` | `{"field": {"not_exists": true}}` | `field IS NULL` |

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

## Simple Filters vs Where Clause

The `filters` map field supports simple `key=value` equality only. For operator-based filtering, use the `where` parameter.

You can use both — they're merged with AND:

```bash
grpcurl -plaintext -d '{
    "collection": "posts",
    "filters": { "status": "published" },
    "where": "{\"title\":{\"contains\":\"hello\"}}"
}' localhost:50051 crap.ContentAPI/Find
```
