# Fields

Fields define the schema of a collection or global. Each field maps to a SQLite column (except arrays and has-many relationships, which use join tables).

## Defining Fields

There are two ways to define fields: **factory functions** (recommended) and **plain tables**.

### Factory Functions (Recommended)

`crap.fields.*` functions set the `type` automatically and return a plain table. Your editor shows only the properties relevant to each field type — no `blocks` on a text field, no `options` on a checkbox.

```lua
fields = {
    crap.fields.text({ name = "title", required = true }),
    crap.fields.select({ name = "status", options = {
        { label = "Draft", value = "draft" },
        { label = "Published", value = "published" },
    }}),
    crap.fields.relationship({ name = "author", relationship = { collection = "users" } }),
}
```

### Plain Tables

You can also define fields as plain tables with an explicit `type` key. This is fully supported and equivalent — factories just set `type` for you.

```lua
fields = {
    { name = "title", type = "text", required = true },
    { name = "status", type = "select", options = { ... } },
}
```

Both syntaxes can be freely mixed in the same `fields` array.

> **Why factories?** The `types/crap.lua` file ships per-type LuaLS classes (e.g., `crap.SelectField`, `crap.ArrayField`). When you use `crap.fields.select({...})`, your editor autocompletes only the properties that apply to select fields. With plain tables, the single `crap.FieldDefinition` class shows every possible property.

## Common Properties

Every field type accepts these properties:

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `name` | string | **required** | Column name. Must be a valid SQL identifier (alphanumeric + underscore). |
| `required` | boolean | `false` | Validation: must have a non-empty value on create/update. |
| `unique` | boolean | `false` | Unique constraint. Checked in the current transaction. For [localized](../locale/overview.md#unique--localized) fields, enforced per locale. |
| `index` | boolean | `false` | Create a B-tree index on this column. Skipped when `unique = true` (already indexed by SQLite). |
| `localized` | boolean | `false` | Enable per-locale values. Requires [localization](../locale/overview.md) to be configured. |
| `validate` | string | `nil` | Lua function ref for custom validation (see below). |
| `default_value` | any | `nil` | Default value applied on create if no value provided. |
| `admin` | table | `{}` | Admin UI display options. |
| `hooks` | table | `{}` | Per-field lifecycle hooks. |
| `access` | table | `{}` | Per-field access control. |

## Supported Types

| Type | SQLite Column | Description |
|------|---------------|-------------|
| `text` | TEXT | Single-line string (`has_many` for tag input) |
| `number` | REAL | Integer or float (`has_many` for tag input) |
| `textarea` | TEXT | Multi-line text |
| `richtext` | TEXT | Rich text (HTML string) |
| `select` | TEXT | Single value from predefined options |
| `radio` | TEXT | Single value from predefined options (radio button UI) |
| `checkbox` | INTEGER | Boolean (0 or 1) |
| `date` | TEXT | Date/datetime/time/month with `picker_appearance` control |
| `email` | TEXT | Email address |
| `json` | TEXT | Arbitrary JSON blob |
| `code` | TEXT | Code string with syntax-highlighted editor |
| `relationship` | TEXT (has-one) or join table (has-many) | Reference to one or more collections; supports polymorphic (`collection = { "posts", "pages" }`) |
| `array` | join table | Repeatable group of sub-fields |
| `group` | prefixed columns | Visual grouping of sub-fields (no extra table) |
| `upload` | TEXT (has-one) or join table (has-many) | File reference to upload collection; supports `has_many` for multi-file |
| `blocks` | join table | Flexible content blocks with different schemas |
| `join` | _(none)_ | Virtual reverse relationship (read-only, computed at read time) |

## `admin` Properties

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `label` | string \| table | `nil` | UI label (defaults to title-cased field name). Supports [localized strings](../locale/overview.md#admin-label-localization). |
| `placeholder` | string \| table | `nil` | Input placeholder text. Supports [localized strings](../locale/overview.md#admin-label-localization). |
| `description` | string \| table | `nil` | Help text displayed below the input. Supports [localized strings](../locale/overview.md#admin-label-localization). |
| `hidden` | boolean | `false` | Hide from admin UI forms |
| `readonly` | boolean | `false` | Display but don't allow editing |
| `width` | string | `nil` | Field width: `"full"`, `"half"`, or `"third"` |

## Layout Wrappers

Row, Collapsible, and Tabs are **layout wrappers** — they exist only for admin UI grouping. They are transparent at the data layer: sub-fields are promoted as top-level columns with no prefix (unlike [Group](group.md), which creates prefixed columns).

### Nesting

Layout wrappers can be nested inside each other and inside Array/Blocks sub-fields at arbitrary depth:

```lua
crap.fields.array({
    name = "team_members",
    fields = {
        crap.fields.tabs({
            name = "member_tabs",
            tabs = {
                {
                    label = "Personal",
                    fields = {
                        crap.fields.row({
                            name = "name_row",
                            fields = {
                                crap.fields.text({ name = "first_name", required = true }),
                                crap.fields.text({ name = "last_name", required = true }),
                            },
                        }),
                        crap.fields.email({ name = "email" }),
                    },
                },
                {
                    label = "Professional",
                    fields = {
                        crap.fields.text({ name = "job_title" }),
                    },
                },
            },
        }),
    },
})
```

In this example, `first_name`, `last_name`, `email`, and `job_title` all become flat columns in the `{collection}_team_members` join table — the Tabs and Row wrappers are invisible at the data and API layer.

All combinations work: Row inside Tabs, Tabs inside Collapsible, Collapsible inside Row, etc.

### Depth limit

The admin UI rendering caps layout nesting at **5 levels deep**. Beyond this, fields are silently omitted from the form. This limit is a safety guard against infinite recursion — realistic schemas never hit it (5 levels means something like Array → Tabs → Collapsible → Row → Tabs → field).

The data layer (DDL, read, write, versions) has no depth limit.

## Custom Validation

The `validate` property references a Lua function in `module.function` format. The function receives `(value, context)` and returns:

- `nil` or `true` — valid
- `false` — invalid with a generic message
- `string` — invalid with a custom error message

```lua
-- hooks/validators.lua
local M = {}

function M.min_length_3(value, ctx)
    if type(value) == "string" and #value < 3 then
        return ctx.field_name .. " must be at least 3 characters"
    end
end

return M
```

```lua
-- In field definition:
crap.fields.text({ name = "title", validate = "hooks.validators.min_length_3" })
```

The context table contains:

| Field | Type | Description |
|-------|------|-------------|
| `collection` | string | Collection slug |
| `field_name` | string | Name of the field being validated |
| `data` | table | Full document data |
| `user` | table/nil | Authenticated user document (nil if unauthenticated) |
| `ui_locale` | string/nil | Admin UI locale code (e.g., `"en"`, `"de"`) |
