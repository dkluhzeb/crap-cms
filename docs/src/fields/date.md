# Date

Date or datetime field stored as an ISO 8601 string.

## SQLite Storage

`TEXT` column.

## Definition

```lua
{
    name = "published_at",
    type = "date",
    admin = {
        placeholder = "2024-01-01",
    },
}
```

## Admin Rendering

Renders as an `<input type="date">` or datetime input in the admin UI.

## Notes

- Values are stored as plain text strings — no server-side date parsing
- Use ISO 8601 format (`YYYY-MM-DD` or `YYYY-MM-DD HH:MM:SS`) for consistent sorting and filtering
- Comparison operators (`greater_than`, `less_than`) work on the string representation
