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

-- Time only — stored as HH:MM or HH:MM:SS
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
| `"timeOnly"` | `<input type="time">` | `HH:MM[:SS]` | `14:30` |
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

Renders as the appropriate HTML5 input type based on `picker_appearance`. For `dayOnly` and `dayAndTime`, the stored ISO string is automatically converted to the format the HTML input expects (`YYYY-MM-DD` and `YYYY-MM-DDTHH:MM[:SS]` respectively). A stored value that carries seconds is shown with them (the input gets `step="1"`), so an untouched field re-submits exactly what is stored.

## Date Constraints

Use `min_date` and `max_date` to restrict the allowed range. Values are validated server-side and set as HTML `min`/`max` attributes on the input. Each bound must be a `YYYY-MM-DD` string, and `min_date` must not be after `max_date` — anything else is a hard error at load time.

```lua
crap.fields.date({
    name = "event_date",
    min_date = "2026-01-01",
    max_date = "2026-12-31",
})
```

Both values use ISO 8601 format. Dates outside the range produce a validation error. What a bound judges depends on the field:

- **`dayOnly` / `dayAndTime` without a timezone** — the UTC day the value is *stored* as. A datetime with an offset is stored converted to UTC, so `2026-01-01T01:00:00+02:00` is judged as December 31.
- **`dayAndTime` with `timezone = true`** — the local day as entered, in the chosen zone: the day the editor picked.
- **`monthOnly`** — the month: each bound is cut to its month, so `min_date = "2026-03-15"` accepts `2026-03`.
- **`timeOnly`** — a time of day has no date, so `min_date` / `max_date` on a `timeOnly` field is a load error.

## Validation

A date value must be a string (a number such as an epoch is rejected, not stored as text). Non-empty values are validated against recognized date/datetime/time/month formats, and the shape must be one the field's `picker_appearance` can show: `timeOnly` takes `HH:MM[:SS]`, `monthOnly` `YYYY-MM`, `dayOnly`/`dayAndTime` a date or datetime — so an API cannot store a value the editor would blank on the next save. Invalid formats produce a validation error. If `min_date` or `max_date` are set, the value is also checked against those bounds.

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

The naming follows the pattern `{field_name}_tz`. Inside Groups, it becomes `{group}__{field}_tz`. Inside an array row, a blocks row, or a group within a row, the companion is the `{field}_tz` key next to the date in the same row — and the date there is stored as UTC as well, so a timezone date has the same shape wherever it lives.

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

- **Writes**: The zone is written together with its date — the stored UTC value is computed from it, so the two are one unit. An update that sends the date without `<name>_tz` clears the zone (the date is then taken as UTC); to change only the zone, send the date with it. A `null` or empty `<name>_tz` clears it explicitly.
- **Localized fields**: Each locale gets its own `_tz` column (e.g., `start_date_tz__en`)
- **Groups / Rows / Tabs / Collapsible / Arrays**: Companion columns follow the parent field's naming rules
- **Versioning**: Timezone data is included in version snapshots and restored correctly
- **Migration**: Adding `timezone = true` to an existing field creates the `_tz` column via `ALTER TABLE ADD COLUMN` with NULL default. No data migration needed.
- **Lua plugins**: The `timezone` and `default_timezone` properties survive roundtrips through `crap.collections.config.list()` and `crap.collections.define()`

## Notes

- Pure dates are stored with UTC noon (`T12:00:00.000Z`), so reading one back in any timezone within ±12h of UTC shows the same calendar date
- A bare date (`2026-01-15`) written to a `timezone = true` field is that zone's local noon, converted to UTC. For a zone more than 12 hours from UTC — UTC+13 / +14 (e.g. `Pacific/Auckland` in summer, `Pacific/Kiritimati`) or UTC−12 — local noon falls on the neighbouring UTC day (`2026-01-14T22:00:00.000Z` in `Pacific/Kiritimati`)
- Comparison operators (`greater_than`, `less_than`) work correctly on the normalized ISO string representation
- The `picker_appearance` option controls whether the picker shows date-only or date+time

## Filtering

A filter operand is normalized like a written value, except a bare day: `2026-01-15` covers the whole **UTC** day `[2026-01-15T00:00:00.000Z, 2026-01-16T00:00:00.000Z)` instead of standing for its noon. `equals` matches any instant on the day — a `dayOnly` value (stored at noon) and a `dayAndTime` value alike — `greater_than` starts at the next midnight, `less_than_or_equal` includes the whole day, and `in` / `not_in` treat each listed day the same way. An operand with a time keeps its exact comparison.

The day is the UTC day of the stored value, also for a `timezone = true` field (its value is stored in UTC; the `_tz` companion is not consulted — reading each row's local day would need the IANA zone rules inside the query, which SQLite does not have). A value stored for a zone more than 12 hours from UTC can therefore sit on the neighbouring UTC day of its local date (see Notes). To filter a local day, send the zone's midnights with an offset (`greater_than_or_equal = "2026-01-15T00:00:00-05:00"`, `less_than = "2026-01-16T00:00:00-05:00"`). The same rule applies to the `created_at` / `updated_at` timestamps — see [Dates](../query-and-filters/overview.md#dates-a-bare-day-covers-the-whole-day).
