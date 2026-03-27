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

## Timezone Support

Date fields can opt into timezone awareness with `timezone = true`. This stores the user's selected IANA timezone in a companion column and converts between local time and UTC automatically.

### Enabling

```lua
crap.fields.date({
    name = "start_date",
    picker_appearance = "dayAndTime",
    timezone = true,
    default_timezone = "America/New_York",  -- optional pre-selected timezone
})
```

Only `dayAndTime` supports timezones — timezone only makes sense when there's a time component. Using `timezone = true` with `dayOnly`, `timeOnly`, or `monthOnly` emits a warning and is ignored.

### How It Works

1. **Admin UI**: A timezone dropdown appears next to the date input. The user selects a timezone and enters a **local time**.
2. **On save**: The local time is converted to UTC using the selected timezone (via `chrono-tz`). Both the UTC date and the IANA timezone string are stored.
3. **On reload**: The UTC value is converted **back to local time** for display. The user always sees the time they entered — re-saving without changes produces the same UTC value (no drift).

### Storage

Two columns are created:

| Column | Type | Example |
|---|---|---|
| `start_date` | TEXT | `2026-05-02T12:00:00.000Z` (UTC) |
| `start_date_tz` | TEXT | `America/Sao_Paulo` |

The naming follows the pattern `{field_name}_tz`. Inside Groups, it becomes `{group}__{field}_tz`.

### API Responses

Both fields appear in gRPC and MCP responses:

```json
{
  "start_date": "2026-05-02T12:00:00.000Z",
  "start_date_tz": "America/Sao_Paulo"
}
```

The date is always UTC. Frontends convert to local display:

```javascript
const local = new Date(doc.start_date)
    .toLocaleString("en-US", { timeZone: doc.start_date_tz });
```

### Global Default Timezone

Set a default timezone for all date fields in `crap.toml`:

```toml
[admin]
default_timezone = "America/New_York"
```

This pre-selects the timezone in the admin dropdown for any date field with `timezone = true` that doesn't specify its own `default_timezone`. The field-level setting takes precedence.

### Compatibility

- **Localized fields**: Each locale gets its own `_tz` column (e.g., `start_date_tz__en`)
- **Groups / Rows / Tabs / Collapsible / Arrays**: Companion columns follow the parent field's naming rules
- **Versioning**: Timezone data is included in version snapshots and restored correctly
- **Migration**: Adding `timezone = true` to an existing field creates the `_tz` column via `ALTER TABLE ADD COLUMN` with NULL default. No data migration needed.
- **Lua plugins**: The `timezone` and `default_timezone` properties survive roundtrips through `crap.collections.config.list()` and `crap.collections.define()`

## Notes

- Pure dates are stored with UTC noon (`T12:00:00.000Z`) so timezone offsets up to ±12h never flip the calendar date
- When `timezone = true` with `dayOnly`, noon is calculated in the selected timezone then converted to UTC
- Comparison operators (`greater_than`, `less_than`) work correctly on the normalized ISO string representation
- The `picker_appearance` option controls whether the picker shows date-only or date+time
