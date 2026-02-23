# Blocks

Flexible content field with multiple block types. Each block type has its own schema. Stored in a join table with JSON data.

## Storage

Blocks fields use a dedicated join table: `{collection}_{field}`.

The join table has columns:

| Column | Type | Description |
|--------|------|-------------|
| `id` | TEXT PRIMARY KEY | Nanoid for each row |
| `parent_id` | TEXT NOT NULL | Foreign key to the parent document |
| `_order` | INTEGER NOT NULL | Sort order (0-indexed) |
| `_block_type` | TEXT NOT NULL | Block type identifier |
| `data` | TEXT NOT NULL | JSON object containing the block's field values |

Unlike arrays (which have typed columns per sub-field), blocks use a single JSON `data` column because each block type can have a different schema.

## Definition

```lua
{
    name = "content",
    type = "blocks",
    blocks = {
        {
            type = "hero",
            label = "Hero Section",
            fields = {
                { name = "heading", type = "text", required = true },
                { name = "subheading", type = "text" },
                { name = "image_url", type = "text" },
            },
        },
        {
            type = "richtext",
            label = "Rich Text",
            fields = {
                { name = "body", type = "richtext" },
            },
        },
        {
            type = "cta",
            label = "Call to Action",
            fields = {
                { name = "text", type = "text", required = true },
                { name = "url", type = "text", required = true },
                { name = "style", type = "select", options = {
                    { label = "Primary", value = "primary" },
                    { label = "Secondary", value = "secondary" },
                }},
            },
        },
    },
}
```

## Block Definitions

Each block definition has:

| Property | Type | Description |
|----------|------|-------------|
| `type` | string | **Required.** Block type identifier. |
| `label` | string | Display label (defaults to type name). |
| `fields` | FieldDefinition[] | Fields within this block type. |

## API Representation

In API responses, blocks fields appear as a JSON array of objects, each with `_block_type` and the block's field values:

```json
{
  "content": [
    {
      "id": "abc123",
      "_block_type": "hero",
      "heading": "Welcome",
      "subheading": "To our site"
    },
    {
      "id": "def456",
      "_block_type": "richtext",
      "body": "<p>Some content...</p>"
    }
  ]
}
```

## Writing Blocks Data

Via gRPC, pass an array of objects with `_block_type`:

```json
{
  "content": [
    { "_block_type": "hero", "heading": "Welcome", "subheading": "To our site" },
    { "_block_type": "richtext", "body": "<p>Content here</p>" }
  ]
}
```

On write, all existing block rows for the parent are deleted and replaced. This is a full replacement, not a merge.

## Admin Rendering

Renders as a repeatable fieldset with:
- A block type selector dropdown
- "Add Block" button that creates a new row of the selected type
- Each row shows the block type label, expand/collapse, and remove button
- Block-specific fields rendered within each row
