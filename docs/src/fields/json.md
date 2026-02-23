# JSON

Arbitrary JSON data stored as a text blob.

## SQLite Storage

`TEXT` column containing a JSON string.

## Definition

```lua
{
    name = "metadata",
    type = "json",
    admin = {
        description = "Arbitrary JSON metadata",
    },
}
```

## Admin Rendering

Renders as a `<textarea>` with monospace font for JSON editing.

## Notes

- Values are stored as raw JSON strings
- No schema validation is performed on the JSON content
- Use hooks or custom `validate` functions to enforce structure if needed
