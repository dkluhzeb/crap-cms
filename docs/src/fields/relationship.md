# Relationship

Reference to documents in another collection. Supports has-one (single reference) and has-many (multiple references via junction table).

## Has-One

Stores a single document ID as a `TEXT` column on the parent table.

```lua
{
    name = "author",
    type = "relationship",
    relationship = {
        collection = "users",
        has_many = false,  -- default
    },
}
```

At `depth=0`, the field value is a string ID. At `depth=1+`, it's replaced with the full document object.

## Has-Many

Uses a junction table (`{collection}_{field}`) with `parent_id`, `related_id`, and `_order` columns.

```lua
{
    name = "tags",
    type = "relationship",
    relationship = {
        collection = "tags",
        has_many = true,
    },
}
```

At `depth=0`, the field value is an array of string IDs. At `depth=1+`, each ID is replaced with the full document object.

## Relationship Config

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `collection` | string | **required** | Target collection slug |
| `has_many` | boolean | `false` | Use a junction table for many-to-many |
| `max_depth` | integer | `nil` | Per-field cap on population depth |

## Junction Table Schema

For a has-many field `tags` on collection `posts`, the junction table is:

```sql
CREATE TABLE posts_tags (
    parent_id TEXT NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
    related_id TEXT NOT NULL,
    _order INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (parent_id, related_id)
);
```

## Admin Rendering

Has-one renders as a searchable select dropdown. Has-many renders as a multi-select with drag-and-drop ordering.

## Population Depth

See [Population Depth](../relationships/population-depth.md) for details on controlling how deeply relationships are resolved.
