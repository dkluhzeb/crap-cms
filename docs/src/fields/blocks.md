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
| `label_field` | string | Sub-field name to use as row label for this block type. |
| `group` | string | Group name for organizing blocks in the picker dropdown. |
| `image_url` | string | Image URL for icon/thumbnail in the block picker. |
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

## Row Labels

By default, block rows display the block type label and row index (e.g., "Hero Section 0"). You can customize this per block type with `label_field`, or across all block types with `row_label`.

### Per-Block `label_field`

Set `label_field` on each block definition to a sub-field name. The value of that field is used as the row title, and updates live as you type.

```lua
{
    name = "content",
    type = "blocks",
    blocks = {
        {
            type = "hero",
            label = "Hero Section",
            label_field = "heading",
            fields = {
                { name = "heading", type = "text", required = true },
                { name = "subheading", type = "text" },
            },
        },
        {
            type = "image",
            label = "Image",
            label_field = "caption",
            fields = {
                { name = "image", type = "upload", relationship = { collection = "media" } },
                { name = "caption", type = "text" },
            },
        },
    },
}
```

Each block type can have a different `label_field` — hero blocks show the heading, image blocks show the caption.

### `row_label` (Lua function)

For computed labels across all block types, set `admin.row_label` on the blocks field. The function receives the full row data (including `_block_type`) and returns a display string.

```lua
-- collections/posts.lua
{
    name = "content",
    type = "blocks",
    admin = {
        row_label = "labels.content_block_row",
    },
    blocks = { ... },
}
```

```lua
-- hooks/labels.lua
local M = {}

function M.content_block_row(row)
    if row._block_type == "hero" then
        return "Hero: " .. (row.heading or "Untitled")
    elseif row._block_type == "code" then
        local lang = row.language or ""
        if lang ~= "" then return "Code (" .. lang .. ")" end
        return "Code"
    end
    return nil -- fall back to per-block label_field or default
end

return M
```

### Priority

1. `row_label` Lua function (if set and returns a non-empty string)
2. Per-block `label_field` on the `BlockDefinition`
3. Field-level `admin.label_field` (shared across all block types)
4. Default: block type label + row index (e.g., "Hero Section 0")

> **Note:** `row_label` is only evaluated server-side. Rows added via JavaScript in the browser fall back to `label_field` (live-updated) or the default until the form is saved and reloaded.

## Row Limits (`min_rows` / `max_rows`)

Enforce minimum and maximum block counts. These are validation constraints, not just UI hints.

```lua
{
    name = "content",
    type = "blocks",
    min_rows = 1,
    max_rows = 20,
    blocks = { ... },
}
```

- **`min_rows`**: Minimum number of blocks. Validated on create/update (skipped for draft saves).
- **`max_rows`**: Maximum number of blocks. Validated on create/update. The admin UI disables the "Add Block" button when the limit is reached.

## Default Collapsed State (`init_collapsed`)

Render existing block rows collapsed by default on page load. New blocks added via the UI are always expanded.

```lua
{
    name = "content",
    type = "blocks",
    admin = {
        init_collapsed = true,
    },
    blocks = { ... },
}
```

## Custom Labels (`labels`)

Customize the "Add Block" button text with singular/plural labels.

```lua
{
    name = "content",
    type = "blocks",
    admin = {
        labels = { singular = "Section", plural = "Sections" },
    },
    blocks = { ... },
}
```

With this config, the add button reads "Add Section" instead of "Add Block".

## Block Groups

Organize blocks into groups in the picker dropdown using `<optgroup>` elements. Ungrouped blocks appear at the top.

```lua
{
    name = "content",
    type = "blocks",
    blocks = {
        {
            type = "hero",
            label = "Hero Section",
            group = "Layout",
            fields = { ... },
        },
        {
            type = "columns",
            label = "Columns",
            group = "Layout",
            fields = { ... },
        },
        {
            type = "richtext",
            label = "Rich Text",
            group = "Content",
            fields = { ... },
        },
        {
            type = "divider",
            label = "Divider",
            -- No group: appears at the top of the dropdown
            fields = {},
        },
    },
}
```

## Card Picker

By default, blocks use a dropdown select to choose the block type. Set `admin.picker = "card"` to use a visual card grid instead. This is useful when you have several block types and want a more visual picker.

```lua
{
    name = "content",
    type = "blocks",
    admin = {
        picker = "card",
    },
    blocks = {
        {
            type = "hero",
            label = "Hero Section",
            fields = { ... },
        },
        {
            type = "richtext",
            label = "Rich Text",
            fields = { ... },
        },
    },
}
```

Each card shows the block type label and a generic icon. To display custom icons or thumbnails, set `image_url` on individual block definitions:

```lua
blocks = {
    {
        type = "hero",
        label = "Hero Section",
        image_url = "/static/blocks/hero.svg",
        fields = { ... },
    },
    {
        type = "richtext",
        label = "Rich Text",
        image_url = "/static/blocks/text.svg",
        fields = { ... },
    },
}
```

Blocks without an `image_url` show a generic widget icon. Both `group` and `image_url` can be combined with the card picker.

## Admin Rendering

Renders as a repeatable fieldset with:
- Drag handle for drag-and-drop reordering
- Row count badge showing the number of blocks
- Collapse/expand all buttons
- Block type selector dropdown with "Add Block" button (or custom label)
- Each row shows the block type label (or custom label), move up/down, duplicate, and remove buttons
- "No items yet" empty state when no blocks exist
- Block-specific fields rendered within each row
- Add button disabled when `max_rows` is reached
