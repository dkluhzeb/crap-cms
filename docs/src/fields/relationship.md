# Relationship

Reference to documents in another collection. Supports has-one (single reference) and has-many (multiple references via junction table).

## Has-One

Stores a single document ID as a `TEXT` column on the parent table.

```lua
crap.fields.relationship({
    name = "author",
    relationship = {
        collection = "users",
        has_many = false,  -- default
    },
})
```

At `depth=0`, the field value is a string ID. At `depth=1+`, it's replaced with the full document object.

## Has-Many

Uses a junction table (`{collection}_{field}`) with `parent_id`, `related_id`, and `_order` columns.

```lua
crap.fields.relationship({
    name = "tags",
    relationship = {
        collection = "tags",
        has_many = true,
    },
})
```

At `depth=0`, the field value is an array of string IDs. At `depth=1+`, each ID is replaced with the full document object.

## Polymorphic Relationships

A relationship field can reference documents from multiple collections by setting `collection` to a Lua array of slugs instead of a single string.

```lua
crap.fields.relationship({
    name = "related_content",
    relationship = {
        collection = { "posts", "pages" },
        has_many = false,
    },
})
```

**Has-one storage** — the column stores a composite string in `"collection/id"` format (e.g., `"posts/abc123"`). At `depth=0` the raw composite string is returned. At `depth=1+` it is replaced with the full document object.

**Has-many storage** — uses a junction table (same as a regular has-many) with an additional `related_collection` TEXT column that records which collection each referenced document belongs to.

```lua
crap.fields.relationship({
    name = "featured_items",
    relationship = {
        collection = { "posts", "pages", "events" },
        has_many = true,
    },
})
```

**Admin UI** — the relationship picker fetches and displays search results grouped by collection, so editors can find and select documents from any of the target collections in one widget.

## Relationship Config

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `collection` | string \| string[] | **required** | Target collection slug, or an array of slugs for polymorphic relationships |
| `has_many` | boolean | `false` | Use a junction table for many-to-many |
| `max_depth` | integer | `nil` | Per-field cap on population depth |

### Legacy Flat Syntax (Deprecated)

A flat syntax is still supported but **deprecated** — a warning is logged at startup when it's used:

```lua
-- Deprecated: triggers a warning
crap.fields.relationship({
    name = "author",
    relation_to = "users",
    has_many = false,
})

-- Preferred: use the relationship table
crap.fields.relationship({
    name = "author",
    relationship = { collection = "users" },
})
```

The flat syntax does not support `max_depth` or polymorphic collections. Migrate to the `relationship = { ... }` table form.

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

Has-one renders as a searchable input. Has-many renders as a multi-select with chips for selected items.

## Drawer Picker

Add `admin.picker = "drawer"` to enable a browse button next to the search input. Clicking it opens a slide-in drawer panel with a searchable list for browsing documents.

```lua
crap.fields.relationship({
    name = "author",
    relationship = { collection = "users" },
    admin = { picker = "drawer" },
})
```

- Without `picker`: inline search autocomplete only (default behavior)
- With `picker = "drawer"`: inline search + browse button that opens a drawer with a scrollable list

## Population Depth

See [Population Depth](../relationships/population-depth.md) for details on controlling how deeply relationships are resolved.
