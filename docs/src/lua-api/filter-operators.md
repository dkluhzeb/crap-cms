# Filter Operators

Filters are used in `crap.collections.find()` queries and in access control constraint returns. They map to SQL WHERE clauses.

## Shorthand: Simple Equality

String values are treated as `equals`:

```lua
{ filters = { status = "published" } }
-- SQL: WHERE status = 'published'
```

## Operator Syntax

Use a table to specify an operator:

```lua
{ filters = { title = { contains = "hello" } } }
-- SQL: WHERE title LIKE '%hello%'
```

## Operator Reference

| Operator | Lua Syntax | SQL | Description |
|----------|-----------|-----|-------------|
| `equals` | `{ equals = "value" }` | `field = ?` | Exact match |
| `not_equals` | `{ not_equals = "value" }` | `field != ?` | Not equal |
| `like` | `{ like = "pattern%" }` | `field LIKE ?` | SQL LIKE pattern |
| `contains` | `{ contains = "text" }` | `field LIKE '%text%'` | Substring match |
| `greater_than` | `{ greater_than = "10" }` | `field > ?` | Greater than |
| `less_than` | `{ less_than = "10" }` | `field < ?` | Less than |
| `greater_than_or_equal` | `{ greater_than_or_equal = "10" }` | `field >= ?` | Greater than or equal |
| `less_than_or_equal` | `{ less_than_or_equal = "10" }` | `field <= ?` | Less than or equal |
| `in` | `{ ["in"] = { "a", "b" } }` | `field IN (?, ?)` | Value in list |
| `not_in` | `{ not_in = { "a", "b" } }` | `field NOT IN (?, ?)` | Value not in list |
| `exists` | `{ exists = true }` | `field IS NOT NULL` | Field is not null |
| `not_exists` | `{ not_exists = true }` | `field IS NULL` | Field is null |

> **Note:** `in` is a Lua keyword, so use `["in"]` bracket syntax.

## Examples

```lua
-- Published posts containing "hello"
crap.collections.find("posts", {
    filters = {
        status = "published",
        title = { contains = "hello" },
    },
})

-- Posts created after a date
crap.collections.find("posts", {
    filters = {
        created_at = { greater_than = "2024-01-01" },
    },
})

-- Posts with specific statuses
crap.collections.find("posts", {
    filters = {
        status = { ["in"] = { "draft", "published" } },
    },
})

-- Posts without a category
crap.collections.find("posts", {
    filters = {
        category = { not_exists = true },
    },
})

-- Posts with wildcard title match
crap.collections.find("posts", {
    filters = {
        title = { like = "Hello%" },
    },
})
```

## Multiple Filters

Multiple filters are combined with AND:

```lua
crap.collections.find("posts", {
    filters = {
        status = "published",
        created_at = { greater_than = "2024-01-01" },
        title = { contains = "update" },
    },
})
-- SQL: WHERE status = ? AND created_at > ? AND title LIKE ?
```

## Value Types

Filter values are always converted to strings for SQL parameter binding. Numbers and booleans are stringified:

```lua
{ filters = { count = 42 } }       -- equals "42"
{ filters = { active = true } }    -- equals "true"
```
