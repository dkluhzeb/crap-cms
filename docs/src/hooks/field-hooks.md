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
| `id` | string/nil | Document id on `update` / read; nil on `create` |
| `data` | table | The **nearest scope** (read-only snapshot): the group object for a field inside a group, the current row for a field inside an array/blocks row, or the full document at the top level. Groups are nested objects (`ctx.data.title` for a `seo.title` sub-field) |
| `document` | table | The **full document** being written or read — a read-only snapshot taken before any field hook in this pass ran (it does not reflect earlier field hooks' changes). Matches `data` at the top level; for a sub-field hook inside an array/blocks row it's the parent document, so the hook can cross-reference fields outside its row |
| `user` | table/nil | Authenticated user document (nil if unauthenticated) |
| `ui_locale` | string/nil | Admin UI locale code (e.g., `"en"`, `"de"`) |
| `locale` | string/nil | Content locale this operation targets (nil when localization is disabled). Distinct from `ui_locale` — this is the locale of the data being written or read |

### Typed Contexts

The type generator (`crap-cms typegen`) emits per-collection field hook contexts
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

### `after_read` with `locale = "all"`

When a read requests **all locales** (`locale = "all"`), a localized
field's value is a per-locale **map** (`{ en = "Hi", de = "Hallo" }`),
not a single scalar — and `ctx.locale` is `nil` for that read (there is
no single target locale). An `after_read` field hook must handle both
shapes: return the map unchanged (or a transformed map) rather than
replacing it with a scalar, which would drop every other locale's value.
For single-locale reads the value is the plain scalar and `ctx.locale`
is that locale, as usual.

## Nesting

Field hooks fire for **every field at every nesting depth**, not just top-level
fields. A hook attached to a sub-field inside a group, an array row, a blocks
row, or any combination (`array → group`, `blocks → group`, array-in-array,
group-in-array, …) runs once per occurrence of that field in the data.

`ctx.data` narrows to the **nearest enclosing scope** at each level, while
`ctx.document` always stays the full document:

| Field location | `ctx.data` is… | `ctx.document` is… |
|----------------|----------------|--------------------|
| Top level | the full document | the full document |
| Inside a group `seo` | the `seo` object | the full document |
| Inside an array/blocks row | that row's object | the full document |
| A sub-field nested deeper | its nearest group/row object | the full document |

So a hook on a field inside `blocks → group` sees the group object as `ctx.data`
and can still reach sibling top-level fields via `ctx.document`. Layout wrappers
(Row, Collapsible, Tabs) are transparent — they don't introduce a new scope.

## Which fields can carry a hook

A field hook fires on the field's **value**. Scalar fields, `group`, `array`,
and `blocks` all carry a value, so a hook on any of them runs:

- `group` — the hook runs on the whole nested object, then sub-field hooks run
  within it.
- `array` / `blocks` — the hook runs on the whole list, then sub-field hooks run
  per row.

The transparent layout wrappers (`row`, `collapsible`, `tabs`) have **no value
of their own** — they only group child fields visually. Placing a lifecycle hook
directly on one is a configuration error and is **rejected at parse time**; put
the hook on a child field instead.

## Definition

```lua
crap.fields.text({
    name = "title",
    hooks = {
        before_validate = { "hooks.fields.trim" },
        before_change = { "hooks.fields.sanitize_html" },
        after_read = { "hooks.fields.add_word_count" },
    },
})
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
