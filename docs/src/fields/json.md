# JSON

Arbitrary JSON data stored as a text blob.

## SQLite Storage

`TEXT` column containing the JSON text; every read returns the **parsed** value (object, list, number …) at any nesting depth — in version snapshots, drafts and live events too. Legacy text that does not parse as JSON is returned as a string.

## Definition

```lua
crap.fields.json({
    name = "metadata",
    admin = {
        description = "Arbitrary JSON metadata",
    },
})
```

## Admin Rendering

Renders as a `<textarea>` with monospace font for JSON editing.

## Notes

- Values are stored as JSON text and read back parsed; the admin editor shows the value pretty-printed and stores what it parses to, so an untouched field round-trips unchanged
- Filters on the column (`meta.a` dot paths) read the stored text
- No schema validation is performed on the JSON content
- Use hooks or custom `validate` functions to enforce structure if needed
