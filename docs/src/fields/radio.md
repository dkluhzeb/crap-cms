# Radio

Single-value selection from predefined options, rendered as radio buttons.

## SQLite Storage

`TEXT` column storing the selected `value`.

## Definition

```lua
crap.fields.radio({
    name = "priority",
    required = true,
    options = {
        { label = "Low", value = "low" },
        { label = "Medium", value = "medium" },
        { label = "High", value = "high" },
    },
})
```

## Options Format

Each option is a table with:

| Property | Type | Description |
|----------|------|-------------|
| `label` | string | Display text in the admin UI |
| `value` | string | Stored value in the database |

## Admin Rendering

Renders as a group of radio buttons (one selectable at a time). Functionally identical to [Select](select.md) but with a different UI presentation — use radio when there are few options and you want them all visible at once.
