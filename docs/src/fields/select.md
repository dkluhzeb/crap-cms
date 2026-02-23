# Select

Single-value selection from predefined options.

## SQLite Storage

`TEXT` column storing the selected `value`.

## Definition

```lua
{
    name = "status",
    type = "select",
    required = true,
    default_value = "draft",
    options = {
        { label = "Draft", value = "draft" },
        { label = "Published", value = "published" },
        { label = "Archived", value = "archived" },
    },
}
```

## Options Format

Each option is a table with:

| Property | Type | Description |
|----------|------|-------------|
| `label` | string | Display text in the admin UI |
| `value` | string | Stored value in the database |

## Admin Rendering

Renders as a `<select>` dropdown.
