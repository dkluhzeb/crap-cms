# Date

Date, datetime, time, or month field with configurable picker appearance and automatic normalization.

## SQLite Storage

`TEXT` column. Values are normalized on write (see Storage Format below).

## Definition

```lua
-- Date only (default) — stored as UTC noon to prevent timezone drift
crap.fields.date({ name = "birthday" })
crap.fields.date({ name = "birthday", picker_appearance = "dayOnly" })

-- Date and time — stored as full ISO 8601 UTC
crap.fields.date({ name = "published_at", picker_appearance = "dayAndTime" })

-- Time only — stored as HH:MM
crap.fields.date({ name = "reminder", picker_appearance = "timeOnly" })

-- Month only — stored as YYYY-MM
crap.fields.date({ name = "birth_month", picker_appearance = "monthOnly" })
```

## Picker Appearance

The `picker_appearance` option controls the HTML input type in the admin UI and how values are stored:

| Value | HTML Input | Storage Format | Example |
|---|---|---|---|
| `"dayOnly"` (default) | `<input type="date">` | `YYYY-MM-DDT12:00:00.000Z` | `2026-01-15T12:00:00.000Z` |
| `"dayAndTime"` | `<input type="datetime-local">` | `YYYY-MM-DDTHH:MM:SS.000Z` | `2026-01-15T09:30:00.000Z` |
| `"timeOnly"` | `<input type="time">` | `HH:MM` | `14:30` |
| `"monthOnly"` | `<input type="month">` | `YYYY-MM` | `2026-01` |

## Date Normalization

All date values are normalized in `coerce_value` before writing to the database, regardless of how they arrive (admin form or gRPC API):

- **Date only** (`2026-01-15`) → `2026-01-15T12:00:00.000Z` (UTC noon prevents timezone drift)
- **Full ISO 8601** (`2026-01-15T09:00:00Z`, `2026-01-15T09:00:00+05:00`) → converted to UTC, formatted as `YYYY-MM-DDTHH:MM:SS.000Z`
- **datetime-local** (`2026-01-15T09:00`) → treated as UTC, formatted as `YYYY-MM-DDTHH:MM:SS.000Z`
- **Time only** (`14:30`) → stored as-is
- **Month only** (`2026-01`) → stored as-is

This normalization ensures consistent storage and correct behavior when filtering and sorting.

## Admin Rendering

Renders as the appropriate HTML5 input type based on `picker_appearance`. For `dayOnly` and `dayAndTime`, the stored ISO string is automatically converted to the format the HTML input expects (`YYYY-MM-DD` and `YYYY-MM-DDTHH:MM` respectively).

## Date Constraints

Use `min_date` and `max_date` to restrict the allowed range. Values are validated server-side and set as HTML `min`/`max` attributes on the input.

```lua
crap.fields.date({
    name = "event_date",
    min_date = "2026-01-01",
    max_date = "2026-12-31",
})
```

Both values use ISO 8601 format. Dates outside the range produce a validation error.

## Validation

Non-empty date values are validated against recognized date/datetime/time/month formats. Invalid formats produce a validation error. If `min_date` or `max_date` are set, the value is also checked against those bounds.

## Notes

- Pure dates are stored with UTC noon (`T12:00:00.000Z`) so timezone offsets up to ±12h never flip the calendar date
- Comparison operators (`greater_than`, `less_than`) work correctly on the normalized ISO string representation
- The `picker_appearance` option controls whether the picker shows date-only or date+time
