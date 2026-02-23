# Relationships

Relationship fields reference documents in other collections. Crap CMS supports two relationship types:

## Has-One

A single reference stored as a `TEXT` column containing the related document's ID.

```lua
{
    name = "author",
    type = "relationship",
    relationship = {
        collection = "users",
    },
}
```

At `depth=0` (default for `Find`):

```json
{ "author": "user_abc123" }
```

At `depth=1`:

```json
{
    "author": {
        "id": "user_abc123",
        "collection": "users",
        "name": "Admin User",
        "email": "admin@example.com",
        "created_at": "2024-01-01 00:00:00"
    }
}
```

## Has-Many

Multiple references stored in a junction table (`{collection}_{field}`).

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

At `depth=0`:

```json
{ "tags": ["tag_1", "tag_2", "tag_3"] }
```

At `depth=1`:

```json
{
    "tags": [
        { "id": "tag_1", "collection": "tags", "name": "Rust" },
        { "id": "tag_2", "collection": "tags", "name": "CMS" },
        { "id": "tag_3", "collection": "tags", "name": "Lua" }
    ]
}
```

## Writing Relationships

### Has-One

Set the field to the related document's ID:

```json
{ "author": "user_abc123" }
```

### Has-Many

Pass an array of IDs:

```json
{ "tags": ["tag_1", "tag_2", "tag_3"] }
```

Order is preserved via the `_order` column in the junction table.

On write, all existing junction table rows for the parent are deleted and replaced. This is a full replacement.

## Partial Updates

For has-many fields, if the field is **absent** from the update data, the junction table is **not modified**. This supports partial updates — only explicitly included fields are changed.
