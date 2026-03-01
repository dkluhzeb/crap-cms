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

## Multi-Value (`has_many`)

Store multiple text values as a JSON array in a TEXT column. Renders as a tag-style input in the admin UI.

```lua
{
    name = "tags",
    type = "text",
    has_many = true,
    min_length = 2,   -- each tag must be at least 2 chars
    max_rows = 10,    -- at most 10 tags
}
```

- Values are stored as `["tag1","tag2","tag3"]` in the TEXT column
- `min_length` / `max_length` validate each individual value
- `min_rows` / `max_rows` validate the count of values
- Duplicate values are prevented in the admin UI
- Type generation maps to `string[]` / `Vec<String>` / `list[str]` etc.

## Admin Rendering

Renders as an `<input type="text">` element. When `has_many = true`, renders as a tag input where users type and press Enter to add chips.

## Notes

- Empty strings are stored as `NULL` in SQLite
- Unknown field types default to `text`
