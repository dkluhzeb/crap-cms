# Localization

Crap CMS supports per-field localization, allowing content to be managed in multiple languages. Any field type can be marked `localized`, and the API returns data differently based on a `locale` parameter.

## Configuration

Enable localization by adding a `[locale]` section to `crap.toml`:

```toml
[locale]
default_locale = "en"
locales = ["en", "de", "fr"]
fallback = true
```

| Field | Default | Description |
|-------|---------|-------------|
| `default_locale` | `"en"` | Default locale code. Content without an explicit locale uses this. |
| `locales` | `[]` | Supported locale codes. Empty = localization disabled. |
| `fallback` | `true` | Fall back to default locale value when the requested locale field is NULL. |

When `locales` is empty (the default), localization is completely disabled and all behavior is unchanged.

## Per-Field Opt-In

Mark individual fields as localized in your Lua definitions:

```lua
crap.collections.define("pages", {
    fields = {
        {
            name = "title",
            type = "text",
            required = true,
            localized = true,  -- this field has per-locale values
        },
        {
            name = "slug",
            type = "text",
            required = true,
            -- not localized — single value shared across all locales
        },
    },
})
```

Only fields with `localized = true` are affected. Non-localized fields behave exactly as before.

## Storage

Localized fields use **suffixed columns** in SQLite:

- A field `title` with locales `["en", "de"]` becomes columns `title__en` and `title__de`
- Non-localized fields keep their single column
- `required` is only enforced on the default locale column (`title__en`)
- `unique` checks the locale-specific column being written to (e.g., writing locale `"de"` checks `title__de`)
- Junction tables (arrays, blocks, has-many) get a `_locale` column

### Unique + Localized

When a field has both `unique = true` and `localized = true`, uniqueness is enforced **per locale**. Two documents can have the same value in different locales, but not in the same locale:

```lua
{
    name = "slug",
    type = "text",
    unique = true,
    localized = true,
}
```

| Scenario | Result |
|----------|--------|
| Doc A has `slug__en = "hello"`, Doc B creates with `slug__en = "hello"` | **Rejected** — duplicate in same locale |
| Doc A has `slug__en = "hello"`, Doc B creates with `slug__de = "hello"` | **Allowed** — different locales |
| Writing with no locale parameter | Checks the default locale column |

This also applies to fields inside a localized Group — uniqueness is checked against the fully suffixed column (e.g., `seo__slug__en`).

## API Behavior

All read and write RPCs accept an optional `locale` parameter:

### Reading

| Locale Parameter | Behavior |
|-----------------|----------|
| Omitted | Returns default locale values with flat field names |
| `"en"` or `"de"` | Returns that locale's values with flat field names |
| `"all"` | Returns all locales as nested objects |

**Flat response** (single locale):
```json
{ "title": "Hello World" }
```

**Nested response** (`locale = "all"`):
```json
{ "title": { "en": "Hello World", "de": "Hallo Welt" } }
```

When `fallback = true` and a field is NULL for the requested locale, the default locale value is returned instead.

### Writing

Writes target a single locale. The `locale` parameter determines which locale column to write to:

```bash
# Write German title
grpcurl -plaintext -d '{
  "collection": "pages",
  "id": "abc123",
  "locale": "de",
  "data": { "title": "Hallo Welt" }
}' localhost:50051 crap.ContentAPI/Update
```

Non-localized fields are always written to their single column regardless of the locale parameter.

## Admin UI

When locales are configured, the admin edit page shows a **locale selector** in the sidebar. Clicking a locale tab reloads the form with that locale's data. The save action writes to the selected locale.

When editing in a non-default locale, **non-localized fields are shown as readonly** with a "Shared Field" badge. This prevents accidentally overwriting values that are shared across all locales.

## Lua API

### Locale in CRUD Operations

All Lua CRUD functions accept an optional `locale` parameter:

```lua
-- Find with locale
local result = crap.collections.find("pages", { locale = "de" })

-- Find by ID with locale
local doc = crap.collections.find_by_id("pages", id, { locale = "de" })

-- Create in a specific locale
crap.collections.create("pages", data, { locale = "de" })

-- Update in a specific locale
crap.collections.update("pages", id, data, { locale = "de" })

-- Globals
local settings = crap.globals.get("site_settings", { locale = "de" })
crap.globals.update("site_settings", data, { locale = "de" })
```

### Locale Configuration Access

```lua
-- Check if localization is enabled
if crap.locale.is_enabled() then
    local default = crap.locale.get_default()  -- "en"
    local all = crap.locale.get_all()           -- {"en", "de", "fr"}
end
```

### Hook Context

The locale is available in hook context:

```lua
function M.before_change(ctx)
    if ctx.locale then
        print("Writing to locale: " .. ctx.locale)
    end
    return ctx
end
```

## Admin Label Localization

Field labels, descriptions, placeholders, select option labels, block labels, and collection/global display names can all be localized. Instead of a plain string, provide a table keyed by locale:

```lua
crap.collections.define("pages", {
    labels = {
        singular = { en = "Page", de = "Seite" },
        plural = { en = "Pages", de = "Seiten" },
    },
    fields = {
        {
            name = "title",
            type = "text",
            required = true,
            localized = true,
            admin = {
                label = { en = "Title", de = "Titel" },
                placeholder = { en = "Enter page title", de = "Seitentitel eingeben" },
                description = { en = "The main heading", de = "Die Hauptüberschrift" },
            },
        },
        {
            name = "status",
            type = "select",
            options = {
                { label = { en = "Draft", de = "Entwurf" }, value = "draft" },
                { label = { en = "Published", de = "Veröffentlicht" }, value = "published" },
            },
        },
    },
})
```

Plain strings still work — they're used as-is regardless of locale:

```lua
admin = { label = "Title", placeholder = "Enter title" }
```

The admin UI resolves labels based on `default_locale` from `crap.toml`.

## Admin UI Translations

All built-in admin UI text (buttons, labels, headings, error messages) can be translated. The system uses a `{{t "key"}}` Handlebars helper that looks up translation strings.

### Built-in English

English translations are compiled into the binary. No configuration needed for English.

### Custom Translations

Place a JSON file at `<config_dir>/translations/<locale>.json` to override or add strings:

```json
{
  "save": "Speichern",
  "delete": "Löschen",
  "create": "Neu erstellen",
  "cancel": "Abbrechen",
  "search_placeholder": "Suchen...",
  "collections": "Sammlungen",
  "globals": "Globale",
  "dashboard": "Übersicht"
}
```

The file must match your `default_locale` in `crap.toml`. Keys not present in the override file fall back to English.

### Interpolation

Translation strings support `{{variable}}` placeholders:

```json
{
  "page_of": "Seite {{page}} von {{total}}",
  "no_items_yet": "Keine {{name}} vorhanden"
}
```

Templates pass values as hash parameters: `{{t "page_of" page=page total=total_pages}}`.

### Available Keys

See `translations/en.json` in the source tree for all available translation keys.

## Backward Compatibility

- No `[locale]` config or empty `locales` = feature completely disabled
- No `localized = true` on fields = no locale columns created
- All existing behavior is preserved when localization is not configured
- Plain string labels/descriptions/placeholders work exactly as before
