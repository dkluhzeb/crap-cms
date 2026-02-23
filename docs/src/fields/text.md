# Text

Single-line string field. The most common field type.

## SQLite Storage

`TEXT` column.

## Definition

```lua
{
    name = "title",
    type = "text",
    required = true,
    unique = true,
    default_value = "Untitled",
    admin = {
        placeholder = "Enter title",
        description = "The display title",
    },
}
```

## Admin Rendering

Renders as an `<input type="text">` element.

## Notes

- Empty strings are stored as `NULL` in SQLite
- Unknown field types default to `text`
