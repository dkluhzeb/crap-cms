# Fields

Fields define the schema of a collection or global. Each field maps to a SQLite column (except arrays and has-many relationships, which use join tables).

## Common Properties

Every field type accepts these properties:

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `name` | string | **required** | Column name. Must be a valid SQL identifier (alphanumeric + underscore). |
| `type` | string | `"text"` | Field type. See [supported types](#supported-types). |
| `required` | boolean | `false` | Validation: must have a non-empty value on create/update. |
| `unique` | boolean | `false` | Unique constraint. Checked in the current transaction. |
| `validate` | string | `nil` | Lua function ref for custom validation (see below). |
| `default_value` | any | `nil` | Default value applied on create if no value provided. |
| `options` | SelectOption[] | `{}` | Options for `select` fields. |
| `admin` | table | `{}` | Admin UI display options. |
| `hooks` | table | `{}` | Per-field lifecycle hooks. |
| `access` | table | `{}` | Per-field access control. |
| `relationship` | table | `nil` | Relationship configuration (for `relationship` fields). |
| `fields` | FieldDefinition[] | `{}` | Sub-field definitions (for `array` fields). |

## Supported Types

| Type | SQLite Column | Description |
|------|---------------|-------------|
| `text` | TEXT | Single-line string |
| `number` | REAL | Integer or float |
| `textarea` | TEXT | Multi-line text |
| `richtext` | TEXT | Rich text (HTML string) |
| `select` | TEXT | Single value from predefined options |
| `checkbox` | INTEGER | Boolean (0 or 1) |
| `date` | TEXT | ISO 8601 date or datetime string |
| `email` | TEXT | Email address |
| `json` | TEXT | Arbitrary JSON blob |
| `relationship` | TEXT (has-one) or join table (has-many) | Reference to another collection |
| `array` | join table | Repeatable group of sub-fields |

## `admin` Properties

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `placeholder` | string | `nil` | Input placeholder text |
| `description` | string | `nil` | Help text displayed below the input |
| `hidden` | boolean | `false` | Hide from admin UI forms |
| `readonly` | boolean | `false` | Display but don't allow editing |
| `width` | string | `nil` | Field width: `"full"`, `"half"`, or `"third"` |

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
{ name = "title", type = "text", validate = "hooks.validators.min_length_3" }
```

The context table contains:

| Field | Type | Description |
|-------|------|-------------|
| `collection` | string | Collection slug |
| `field_name` | string | Name of the field being validated |
| `data` | table | Full document data |
