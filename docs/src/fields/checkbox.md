# Checkbox

Boolean field stored as an integer (0 or 1).

## SQLite Storage

`INTEGER` column with `DEFAULT 0`.

## Definition

```lua
{
    name = "published",
    type = "checkbox",
    default_value = false,
}
```

## Admin Rendering

Renders as an `<input type="checkbox">` element.

## Value Coercion

The following string values are treated as `true`: `"on"`, `"true"`, `"1"`, `"yes"`. Everything else (including absence) is `false`.

## Special Behavior

- Absent checkboxes are always treated as `false` (not as a missing/required field)
- The `required` property is effectively ignored for checkboxes — an unchecked checkbox is always valid
- Default value is `0` at the database level
