# Field Hooks

Field-level hooks operate on individual field values rather than the full document context.

## Signature

```lua
function hook(value, context)
    -- transform value
    return new_value
end
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `value` | any | Current field value |
| `context` | table | See context fields below |

**Return value:** The new field value. This replaces the existing value in the data.

## Context Table

| Field | Type | Description |
|-------|------|-------------|
| `field_name` | string | Name of the field being processed |
| `collection` | string | Collection slug |
| `operation` | string | `"create"`, `"update"`, `"find"`, `"find_by_id"` |
| `data` | table | Full document data (read-only snapshot) |

## Events

| Event | CRUD Access | Use Case |
|-------|-------------|----------|
| `before_validate` | Yes | Normalize values before validation (trim, lowercase, etc.) |
| `before_change` | Yes | Transform values after validation (compute derived fields) |
| `after_change` | No | Side effects after write (logging, cache) |
| `after_read` | No | Transform values before response (formatting, computed fields) |

## Definition

```lua
{
    name = "title",
    type = "text",
    hooks = {
        before_validate = { "hooks.fields.trim" },
        before_change = { "hooks.fields.sanitize_html" },
        after_read = { "hooks.fields.add_word_count" },
    },
}
```

## Example

```lua
-- hooks/fields.lua
local M = {}

function M.trim(value, ctx)
    if type(value) == "string" then
        return value:match("^%s*(.-)%s*$")
    end
    return value
end

function M.slugify(value, ctx)
    -- Auto-generate slug from title if empty
    if (value == nil or value == "") and ctx.data.title then
        return crap.util.slugify(ctx.data.title)
    end
    return value
end

return M
```
