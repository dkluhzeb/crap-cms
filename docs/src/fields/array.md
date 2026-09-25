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
| `_locale` | TEXT | Only when the array is [localized](../locale/overview.md#localized-arraysblocksrelationships): the locale each row belongs to |
| *sub-fields* | varies | One column per sub-field |
| `{name}_tz` | TEXT | Companion column for each date sub-field with `timezone = true` |

The table is indexed on `parent_id` (`parent_id, _locale` when localized); see
[Junction Tables](../database/overview.md#junction-tables).

## Definition

```lua
crap.fields.array({
    name = "slides",
    fields = {
        crap.fields.text({ name = "title", required = true }),
        crap.fields.text({ name = "image_url" }),
        crap.fields.textarea({ name = "caption" }),
    },
    admin = {
        description = "Image slides for the gallery",
    },
})
```

## Sub-Fields

Sub-fields support the same properties as regular fields (name, type, required, default_value, admin, etc.), and may themselves be composites — **groups, arrays, blocks, and relationships nest to any depth**.

One locale-related nuance: on a **localized** array, `required` sub-fields are
enforced on every submitted row in **every** locale (rows have no per-field
fallback, so leniency would publish rows with missing required values). See
[Required across locales](../locale/overview.md#required-across-locales).

Scalar sub-fields (and has-one relationships) are stored as columns in the array's join table. Composites nested *inside* an array row — a group, a nested array, a nested blocks field — are stored as JSON within the row (not as their own relational tables), and are still queryable via dot-notation filters (`items.dimensions.width` → `json_extract` / `json_each`; see [Query & Filters](../query-and-filters/overview.md)). An array inside a top-level group has its own join table (`{collection}_{group}__{field}`) and is filtered through the group: `seo.links.url` (or `seo__links.url`). Reference-counting, back-references, validation, and field-level access all descend into this nested JSON at any depth.

### Layout Wrappers in Sub-Fields

Array sub-fields can be organized with [Row](row.md), [Collapsible](collapsible.md), and [Tabs](tabs.md) layout wrappers. These are transparent — their children become flat columns in the join table, exactly as if they were listed directly in `fields`.

```lua
crap.fields.array({
    name = "items",
    fields = {
        crap.fields.tabs({
            name = "item_tabs",
            tabs = {
                {
                    label = "Content",
                    fields = {
                        crap.fields.text({ name = "title", required = true }),
                        crap.fields.textarea({ name = "description" }),
                    },
                },
                {
                    label = "Appearance",
                    fields = {
                        crap.fields.select({ name = "color", options = { ... } }),
                    },
                },
            },
        }),
    },
})
```

The join table gets columns `title`, `description`, and `color` — the Tabs wrapper is invisible at the data layer. Nesting is supported at arbitrary depth (e.g., Row inside Tabs inside Array). See [Layout Wrappers](overview.md#layout-wrappers) for details.

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
    { "id": "k7Qx2Lr9", "title": "Slide 1", "image_url": "/img/1.jpg" },
    { "title": "Slide 2", "image_url": "/img/2.jpg" }
  ]
}
```

A write sends the complete list of rows, and the stored rows are reconciled
against it by row `id` (every row read back carries its `id`):

- A row whose `id` matches an existing row of this document **updates that row
  in place**, setting only the sub-fields it supplies. A sub-field the row
  omits — or one the caller may not write (field-level `update` access) —
  keeps its stored value.
- A row without an `id` (or with an `id` that is not one of this document's
  rows) is **inserted** as a new row with a server-generated id.
- Stored rows missing from the list are **deleted**.
- Row order follows the list order.

So always send back the `id` of each row you read when updating: a row sent
without it is a new row, and any sub-field it does not carry is lost.

## Row Labels

By default, array rows in the admin UI are labeled with the field label and row index (e.g., "Slides 0", "Slides 1"). You can customize this with `label_field` and `row_label`.

### `label_field`

Set `admin.label_field` to the name of a sub-field. Its value is used as the row title, and updates live as you type.

```lua
crap.fields.array({
    name = "slides",
    admin = {
        label_field = "title",
    },
    fields = {
        crap.fields.text({ name = "title", required = true }),
        crap.fields.text({ name = "image_url" }),
        crap.fields.textarea({ name = "caption" }),
    },
})
```

With this configuration, each row shows the `title` value instead of "Slides 0".

### `row_label` (Lua function)

For computed labels, set `admin.row_label` to a Lua function reference. The function receives the row data as a table and returns a display string (or `nil` to fall back to `label_field` or the default).

```lua
-- collections/products.lua
crap.fields.array({
    name = "variants",
    admin = {
        row_label = "labels.variant_row",
        label_field = "name", -- fallback if row_label returns nil
    },
    fields = {
        crap.fields.text({ name = "name", required = true }),
        crap.fields.text({ name = "sku" }),
        crap.fields.number({ name = "price" }),
    },
})
```

```lua
-- hooks/labels.lua
local M = {}

function M.variant_row(row)
    local name = row.name or "Untitled"
    if row.sku and row.sku ~= "" then
        return name .. " (" .. row.sku .. ")"
    end
    return name
end

return M
```

### Priority

1. `row_label` Lua function (if set and returns a non-empty string)
2. `label_field` sub-field value (if set and the field has a value)
3. Default: field label + row index (e.g., "Slides 0")

> **Note:** `row_label` is only evaluated server-side. Rows added via JavaScript in the browser fall back to `label_field` (live-updated) or the default until the form is saved and reloaded.

## Row Limits (`min_rows` / `max_rows`)

Enforce minimum and maximum row counts. These are validation constraints (like `required`), not just UI hints.

```lua
crap.fields.array({
    name = "slides",
    min_rows = 1,
    max_rows = 10,
    fields = { ... },
})
```

- **`min_rows`**: Minimum number of items. Validated on create/update (skipped for draft saves).
- **`max_rows`**: Maximum number of items. Validated on create/update. The admin UI disables the "Add" button when the limit is reached.

Row limits are enforced at every nesting depth — an array nested inside another array, inside blocks, or inside a group is validated the same as a top-level one.

Validation runs in `validate_fields()`, shared by admin handlers, gRPC, and Lua `crap.collections.create()`/`update()`.

## Default Collapsed State (`collapsed`)

Existing rows render collapsed by default on page load (`admin.collapsed = true`). Set `admin.collapsed = false` to start rows expanded. New rows added via the "Add" button are always expanded.

```lua
crap.fields.array({
    name = "slides",
    admin = {
        collapsed = false, -- start rows expanded (default is true)
    },
    fields = { ... },
})
```

## Custom Labels (`labels`)

Customize the row labels: `singular` names one row — the "Add Row" button text and the
header of a row without a `label_field` / `row_label` title — and `plural` is the field
header when `admin.label` is not set.

```lua
crap.fields.array({
    name = "slides",
    admin = {
        labels = { singular = "Slide", plural = "Slides" },
    },
    fields = { ... },
})
```

With this config, the field header reads "Slides", the add button "Add Slide" instead of
"Add Row", and untitled rows "Slide 1", "Slide 2", …

## Admin Rendering

Renders as a repeatable fieldset with:
- Drag handle for drag-and-drop reordering
- Row count badge showing the number of items
- Toggle collapse/expand all button
- Each row has expand/collapse toggle, move up/down, duplicate, and remove buttons
- "No items yet" empty state when no rows exist
- "Add Row" button (or custom label) to append new rows
- Add button disabled when `max_rows` is reached
