# Filter Operators

Filters are used in `crap.collections.find()` queries and in access control constraint returns. They map to SQL WHERE clauses.

## Shorthand: Simple Equality

String values are treated as `equals`:

```lua
{ where = { status = "published" } }
-- SQL: WHERE status = 'published'
```

## Operator Syntax

Use a table to specify an operator:

```lua
{ where = { title = { contains = "hello" } } }
-- SQL: WHERE title LIKE '%hello%'
```

## Operator Reference

| Operator | Lua Syntax | SQL | Description |
|----------|-----------|-----|-------------|
| `equals` | `{ equals = "value" }` | `field = ?` | Exact match |
| `not_equals` | `{ not_equals = "value" }` | `field != ?` | Not equal |
| `like` | `{ like = "pattern%" }` | `field LIKE ?` | SQL LIKE pattern |
| `contains` | `{ contains = "text" }` | `field LIKE '%text%' ESCAPE '\'` | Substring match (wildcards `%` and `_` in the search text are escaped) |
| `greater_than` | `{ greater_than = "10" }` | `field > ?` | Greater than |
| `less_than` | `{ less_than = "10" }` | `field < ?` | Less than |
| `greater_than_or_equal` | `{ greater_than_or_equal = "10" }` | `field >= ?` | Greater than or equal |
| `less_than_or_equal` | `{ less_than_or_equal = "10" }` | `field <= ?` | Less than or equal |
| `in` | `{ ["in"] = { "a", "b" } }` | `field IN (?, ?)` | Value in list |
| `not_in` | `{ not_in = { "a", "b" } }` | `field NOT IN (?, ?)` | Value not in list |
| `exists` | `{ exists = true }` | `field IS NOT NULL` | Field is not null (value is ignored — only the key matters) |
| `not_exists` | `{ not_exists = true }` | `field IS NULL` | Field is null (value is ignored — use `not_exists` for IS NULL, not `{ exists = false }`) |

> **Note:** `in` is a Lua keyword, so use `["in"]` bracket syntax.

## Examples

```lua
-- Published posts containing "hello"
crap.collections.find("posts", {
    where = {
        status = "published",
        title = { contains = "hello" },
    },
})

-- Posts created after a date
crap.collections.find("posts", {
    where = {
        created_at = { greater_than = "2024-01-01" },
    },
})

-- Posts with specific statuses
crap.collections.find("posts", {
    where = {
        status = { ["in"] = { "draft", "published" } },
    },
})

-- Posts without a category
crap.collections.find("posts", {
    where = {
        category = { not_exists = true },
    },
})

-- Posts with wildcard title match
crap.collections.find("posts", {
    where = {
        title = { like = "Hello%" },
    },
})
```

## Multiple Filters

Multiple filters are combined with AND:

```lua
crap.collections.find("posts", {
    where = {
        status = "published",
        created_at = { greater_than = "2024-01-01" },
        title = { contains = "update" },
    },
})
-- SQL: WHERE status = ? AND created_at > ? AND title LIKE ?
```

## OR Groups

Use the `["or"]` key to combine groups of conditions with OR logic. Each element is a table of AND-ed conditions:

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
-- SQL: WHERE (title LIKE '%hello%' OR category = ?)
```

OR can combine with top-level AND filters:

```lua
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
-- SQL: WHERE status = ? AND (title LIKE '%hello%' OR title LIKE '%world%')
```

Each OR element can have multiple fields (AND-ed within the group):

```lua
-- (status = "published" AND title contains "hello") OR (status = "draft")
crap.collections.find("posts", {
    where = {
        ["or"] = {
            { status = "published", title = { contains = "hello" } },
            { status = "draft" },
        },
    },
})
-- SQL: WHERE ((status = ? AND title LIKE '%hello%') OR status = ?)
```

> **Note:** `or` is not a Lua keyword, but `["or"]` bracket syntax is recommended for clarity.

## Nested Field Filters (Dot Notation)

Filter on sub-fields of group, array, blocks, and has-many relationship fields using dot notation:

```lua
-- Group sub-field: seo.meta_title → seo__meta_title column
crap.collections.find("pages", {
    where = { ["seo.meta_title"] = { contains = "SEO" } },
})

-- Array sub-field: find products with any variant color "red"
crap.collections.find("products", {
    where = { ["variants.color"] = "red" },
})

-- Block sub-field: find posts with any content block containing "hello"
crap.collections.find("posts", {
    where = { ["content.body"] = { contains = "hello" } },
})

-- Block type filter
crap.collections.find("posts", {
    where = { ["content._block_type"] = "image" },
})

-- Has-many relationship: find posts with a specific tag
crap.collections.find("posts", {
    where = { ["tags.id"] = "tag-123" },
})
```

Array and block filters use `EXISTS` subqueries — they match parent documents where **at least one** row matches. All filter operators work with dot notation paths.

See [Query & Filters](../query-and-filters/overview.md#nested-field-filters-dot-notation) for the full reference.

## Value Types

Filter values are always converted to strings for SQL parameter binding. Numbers and booleans are stringified:

```lua
{ where = { count = 42 } }       -- equals "42"
{ where = { active = true } }    -- equals "true"
```
