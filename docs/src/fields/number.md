# Number

Numeric field for integers or floating-point values.

## SQLite Storage

`REAL` column. Empty values are stored as `NULL`.

## Definition

```lua
{
    name = "price",
    type = "number",
    required = true,
    default_value = 0,
    admin = {
        placeholder = "0.00",
    },
}
```

## Admin Rendering

Renders as an `<input type="number">` element.

## Value Coercion

String values from form submissions are parsed as `f64`. If parsing fails, `NULL` is stored.
