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
| `user` | table/nil | Authenticated user document (nil if unauthenticated) |
| `ui_locale` | string/nil | Admin UI locale code (e.g., `"en"`, `"de"`) |

### Typed Contexts

The type generator (`crap-cms types lua`) emits per-collection field hook contexts
with typed `data` fields:

- **Collections:** `crap.field_hook.{PascalCase}` — e.g., `crap.field_hook.Posts`
  has `data: crap.data.Posts`
- **Globals:** `crap.field_hook.global_{slug}` — e.g., `crap.field_hook.global_site_settings`
  has `data: crap.global_data.SiteSettings`

Use the typed context when a hook is specific to one collection:

```lua
---@param value number|nil
---@param context crap.field_hook.Inquiries
---@return number|nil
return function(value, context)
    -- context.data is typed as crap.data.Inquiries
    -- IDE autocompletes context.data.name, context.data.email, etc.
    return value
end
```

For shared hooks that work across multiple collections, use the generic
`crap.FieldHookContext` (where `data` is `table<string, any>`).

## Events

| Event | CRUD Access | Use Case |
|-------|-------------|----------|
| `before_validate` | Yes | Normalize values before validation (trim, lowercase, etc.) |
| `before_change` | Yes | Transform values after validation (compute derived fields) |
| `after_change` | Yes | Side effects after write with CRUD access (logging, cascades) |
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
