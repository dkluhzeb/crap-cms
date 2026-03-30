# Select

Single-value selection from predefined options.

## SQLite Storage

`TEXT` column storing the selected `value`.

## Definition

```lua
crap.fields.select({
    name = "status",
    required = true,
    default_value = "draft",
    options = {
        { label = "Draft", value = "draft" },
        { label = "Published", value = "published" },
        { label = "Archived", value = "archived" },
    },
})
```

## Options Format

Each option is a table with:

| Property | Type | Description |
|----------|------|-------------|
| `label` | string | Display text in the admin UI |
| `value` | string | Stored value in the database |

## Multi-Value (`has_many`)

Allow selecting multiple options. Values are stored as a JSON array in a TEXT column.

```lua
crap.fields.select({
    name = "categories",
    has_many = true,
    options = {
        { label = "News", value = "news" },
        { label = "Tech", value = "tech" },
        { label = "Sports", value = "sports" },
    },
})
```

## Admin Rendering

Renders as a `<select>` dropdown. When `has_many = true`, renders as a multi-select.
