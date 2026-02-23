# Array

Repeatable group of sub-fields. Each array item is a row in a join table.

## Storage

Array fields use a dedicated join table: `{collection}_{field}`.

The join table has columns:

| Column | Type | Description |
|--------|------|-------------|
| `id` | TEXT PRIMARY KEY | Nanoid for each row |
| `parent_id` | TEXT NOT NULL | Foreign key to the parent document |
| `_order` | INTEGER NOT NULL | Sort order (0-indexed) |
| *sub-fields* | varies | One column per sub-field |

## Definition

```lua
{
    name = "slides",
    type = "array",
    fields = {
        { name = "title", type = "text", required = true },
        { name = "image_url", type = "text" },
        { name = "caption", type = "textarea" },
    },
    admin = {
        description = "Image slides for the gallery",
    },
}
```

## Sub-Fields

Sub-fields support the same properties as regular fields (name, type, required, default_value, admin, etc.) but do not support nested arrays or relationships.

## API Representation

In API responses, array fields appear as a JSON array of objects:

```json
{
  "slides": [
    { "id": "abc123", "title": "Slide 1", "image_url": "/img/1.jpg", "caption": "First" },
    { "id": "def456", "title": "Slide 2", "image_url": "/img/2.jpg", "caption": "Second" }
  ]
}
```

## Writing Array Data

Via gRPC, pass an array of objects:

```json
{
  "slides": [
    { "title": "Slide 1", "image_url": "/img/1.jpg" },
    { "title": "Slide 2", "image_url": "/img/2.jpg" }
  ]
}
```

On write, all existing rows for the parent are deleted and replaced with the new data. This is a full replacement, not a merge.

## Admin Rendering

Renders as a repeatable fieldset with add/remove/reorder controls.
