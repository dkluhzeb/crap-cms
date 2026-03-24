# Number

Numeric field for integers or floating-point values.

## SQLite Storage

`REAL` column. Empty values are stored as `NULL`.

## Definition

```lua
crap.fields.number({
    name = "price",
    required = true,
    default_value = 0,
    admin = {
        placeholder = "0.00",
    },
})
```

## Multi-Value (`has_many`)

Store multiple numbers as a JSON array in a TEXT column. Renders as a tag-style input in the admin UI.

```lua
crap.fields.number({
    name = "scores",
    has_many = true,
    min = 0,
    max = 100,
    max_rows = 5,
})
```

- Values are stored as `["10","20","30"]` in the TEXT column
- `min` / `max` validate each individual value
- `min_rows` / `max_rows` validate the count of values
- Type generation maps to `number[]` / `Vec<f64>` / `list[float]` etc.

## Step

Set `admin.step` to control the number input step attribute:

```lua
crap.fields.number({
    name = "price",
    admin = { step = "0.01" },
})
```

Valid values: `"1"` (integers only), `"0.01"` (cents), `"any"` (no step constraint). Defaults to browser default (`"1"`).

## Admin Rendering

Renders as an `<input type="number">` element. When `has_many = true`, renders as a tag input where users type and press Enter to add number chips.

## Value Coercion

String values from form submissions are parsed as `f64`. If parsing fails, `NULL` is stored.
