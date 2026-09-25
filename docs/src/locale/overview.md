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

Locale codes must differ by more than a separator: `pt-BR` and `pt_BR` name
the same `__pt_BR` column and are rejected when the config loads.

## Per-Field Opt-In

Mark individual fields as localized in your Lua definitions:

```lua
crap.collections.define("pages", {
    fields = {
        crap.fields.text({
            name = "title",
            required = true,
            localized = true,  -- this field has per-locale values
        }),
        crap.fields.text({
            name = "slug",
            required = true,
            -- not localized — single value shared across all locales
        }),
    },
})
```

Only fields with `localized = true` are affected. Non-localized fields behave exactly as before.

### Enabling localization on an existing field

Setting `localized = true` on a field that already holds data moves the stored
values into `{field}__{default_locale}` during the next schema sync; the bare
`{field}` column is left behind and can be removed with
`crap-cms db cleanup --confirm`, which names it as an orphan. Clearing
`localized` is the mirror: the default locale's column is copied back into
`{field}` (overwriting whatever the bare column still held), and other locales'
translations stay in their columns — they are not merged. The move happens
whenever the flag flips, not only when a column is first created, so flipping
back after editing keeps the edits.

## Storage

Localized fields use **suffixed columns** in SQLite:

- A field `title` with locales `["en", "de"]` becomes columns `title__en` and `title__de`
- Non-localized fields keep their single column
- Only the default-locale column (`title__en`) is `NOT NULL` for a required field; per-locale completeness is enforced in the validation layer (see [Required across locales](#required-across-locales))
- `unique` checks the locale-specific column being written to (e.g., writing locale `"de"` checks `title__de`)
- Junction tables (arrays, blocks, has-many) get a `_locale` column

### Required across locales

By default a `required` localized field must only be filled in the **default
locale** — untranslated locales fall back, and a translation can be cleared
(set its fields to `nil` in that locale). To require more, set
`required_locales` on the field (or `required_locales` as a collection-level
default):

```lua
crap.fields.text({
    name = "title",
    required = true,
    localized = true,
    required_locales = "all",        -- every configured locale
    -- or a specific set: required_locales = { "en", "de" },
})
```

Completeness is checked on **non-draft** writes against the document's actual
state (submitted data overlaid on what the rest of the write lands), so it acts as a **publish
gate**: you can save incomplete translations as **drafts**, and publishing (or
any live save on a non-versioned collection) requires every locale in
`required_locales` to be filled. To *remove* a translation, clear that locale's
fields with an [update](#removing-a-translation) — you can't clear a field
that's still required in that locale.

`required_locales` only applies to localized fields. It covers both
column-backed fields (text, number, …) and join-backed fields (arrays, blocks,
has-many relationships) — for the latter, "present in a locale" means the
field has at least one row for that locale.

**Sub-fields inside a localized array's rows are different:** a required
sub-field is enforced on **every submitted row, in every locale**. This is
deliberate, not an inconsistency. The default-locale leniency above works
because top-level scalars *fall back* — an untranslated `title` serves the
default locale's value, so a `required` field always reaches API consumers.
Rows have no per-field fallback: once a locale has its own rows they replace
the default set, and an empty required sub-field would surface as a missing
value (and contradict the generated client types, which mark required
sub-fields non-nullable). Translating gradually still works without friction —
a save that submits *no* rows for a locale validates nothing; the rule is only
"if you add a row, finish it". For half-finished rows, save a draft (required
is not enforced on drafts).

Locale codes in `required_locales` are checked against `[locale].locales` at
startup: a typo (e.g. `"de-DE"` when only `"de"` is configured), or setting
`required_locales` while localization is disabled, fails to boot with a clear
error instead of silently breaking every non-draft write.

The check judges the values a write will actually land: on a publish that adopts a pending draft, the draft's other locales (a draft that cleared a required translation cannot be published); on a version restore, the snapshot being restored (a complete snapshot restores over an incomplete live row).

### Unique + Localized

When a field has both `unique = true` and `localized = true`, uniqueness is enforced **per locale**. Two documents can have the same value in different locales, but not in the same locale:

```lua
crap.fields.text({
    name = "slug",
    unique = true,
    localized = true,
})
```

| Scenario | Result |
|----------|--------|
| Doc A has `slug__en = "hello"`, Doc B creates with `slug__en = "hello"` | **Rejected** — duplicate in same locale |
| Doc A has `slug__en = "hello"`, Doc B creates with `slug__de = "hello"` | **Allowed** — different locales |
| Writing with no locale parameter | Checks the default locale column |

This also applies to fields inside a localized Group — uniqueness is checked against the fully suffixed column (e.g., `seo__slug__en`).

### Changing the locale configuration

- **Changing `default_locale`** is allowed and warned at startup, never
  blocked. Existing content stays in the old default's columns; default reads
  switch to the new locale; `fallback` now resolves toward the new default;
  `required` and completeness are judged against the new default. Copy or
  re-save content into the new default locale before switching.
- **Adding or removing a locale** re-runs the reference-count backfill once on
  the next start. A removed locale's columns and join rows are left in place;
  `crap-cms db cleanup` reports them — for collections and globals — and
  removes them with `--confirm`. A
  version taken before a locale was added leaves that locale untouched when
  restored.

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

When `fallback = true` and a field is NULL for the requested locale, the default locale value is returned instead. A localized array, blocks or has-many relationship falls back as a whole: a document holding no row for the requested locale returns the default locale's rows.

Filters match what the read returns. A filter on a localized field compares the
value the response shows, fallback included — a document listed with its
default-locale title is matched by a filter on that title, and a document
listed with its default-locale tags is matched (and, for `not_equals` /
`not_in` / `not_exists`, excluded) by a filter on `tags.id`. With
`locale = "all"`, filters and sorting use the default locale's values and rows.

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

Non-localized (shared) fields are **only writable under the default locale**.
A write with a non-default `locale` parameter is *locale-locked*: shared
fields in the payload are a **validation error naming each field** (they used
to be silently skipped — a success response that discarded data). This
protects the canonical value from being clobbered by a translation edit while
never letting a write half-apply. To change shared fields, write without a
`locale` parameter or with the default locale.

`locale = "all"` is a read shape. A write — `create`, `update`, `create_many`,
`update_many`, `update_global`, `validate`, and an upload write from the admin
form or `POST/PUT /api/upload` — that passes it is rejected with a `locale`
field error; writes target exactly one locale.

### Removing a translation

There is no per-locale *delete* operation — and there doesn't need to be. A
document is one row shared across all locales (localized fields are just
per-locale columns / `_locale`-scoped junction rows), so "removing a
translation" is an **update** that clears that locale's content:

```lua
-- Remove the German translation of a document
crap.collections.pages.update("abc123", {
    title = crap.null,  -- clears title__de
    body = crap.null,   -- clears body__de
    gallery = {},       -- clears the de rows of a localized array/relationship
}, { locale = "de" })
```

Use `crap.null`, not `nil`: a `nil` value in a Lua table constructor is no key
at all, so `{ title = nil }` sends nothing and leaves `title__de` untouched (an
absent key keeps the stored value). Over the wire the same clear is a JSON
`null` (MCP) or a `null_value` (gRPC).

This nulls the `*__de` columns, removes only the `_locale = "de"` junction
rows, and decrements ref-counts for any localized relationships that were
removed. The document still exists; reading it in `de` now falls back to the
default locale (when `fallback = true`). To remove a document entirely (all
locales), use `delete`.

You **cannot** clear a field that's required in that locale — the
[completeness check](#required-across-locales) refuses it on a non-draft write
(the default locale is always required; others depend on `required_locales`).

> **Note:** an update is a full write of the fields you pass, so a *shared*
> (non-localized) `checkbox` you omit is reset to off. When clearing a locale on
> a collection that has shared checkboxes, include their current values in the
> same `update` call.

## Admin UI

When locales are configured, the admin edit page shows a **locale selector** in the sidebar. Clicking a locale tab reloads the form with that locale's data. The save action writes to the selected locale.

When editing in a non-default locale, **non-localized fields are shown as readonly** with a "Shared Field" badge. This prevents accidentally overwriting values that are shared across all locales.

## Lua API

### Locale in CRUD Operations

All Lua CRUD functions accept an optional `locale` parameter:

```lua
-- Find with locale
local result = crap.collections.pages.find({ locale = "de" })

-- Find by ID with locale
local doc = crap.collections.pages.find_by_id(id, { locale = "de" })

-- Create in a specific locale
crap.collections.pages.create(data, { locale = "de" })

-- Update in a specific locale
crap.collections.pages.update(id, data, { locale = "de" })

-- Globals
local settings = crap.globals.site_settings.get({ locale = "de" })
crap.globals.site_settings.update(data, { locale = "de" })
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
        crap.fields.text({
            name = "title",
            required = true,
            localized = true,
            admin = {
                label = { en = "Title", de = "Titel" },
                placeholder = { en = "Enter page title", de = "Seitentitel eingeben" },
                description = { en = "The main heading", de = "Die Hauptüberschrift" },
            },
        }),
        crap.fields.select({
            name = "status",
            options = {
                { label = { en = "Draft", de = "Entwurf" }, value = "draft" },
                { label = { en = "Published", de = "Veröffentlicht" }, value = "published" },
            },
        }),
    },
})
```

Plain strings still work — they're used as-is regardless of locale:

```lua
admin = { label = "Title", placeholder = "Enter title" }
```

A localized label resolves in this order:

1. the viewer's **admin UI locale** (the language picked in the admin header),
2. then `default_locale` from `crap.toml`,
3. then — so a label never renders blank while it has any translation — the
   alphabetically first key it defines.

Labels are admin UI text, so this applies whether or not content localization
(`locales`) is enabled. Within an admin request it covers every label resolved
on the viewer's behalf — the page itself, and `crap.schema` or a label read by
a hook the request runs (a list, the edit page, a create, update, delete,
restore or empty-trash, `before_render`). Surfaces with no viewer — the
gRPC/REST schema endpoints, MCP, and `crap.schema` reads outside an admin
request — resolve against `default_locale`.

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

Each file's name is the UI locale it defines: `de.json` extends the built-in
German, and a new name such as `fr.json` adds that language to the admin's
language picker. Keys not present in a file fall back to English. A file that
cannot be read or is not a flat `"key": "string"` map is skipped with a
warning in the log naming the file and the reason.

### Interpolation

Translation strings support `{{variable}}` placeholders:

```json
{
  "page_of": "Seite {{page}} von {{total}}",
  "no_items_yet": "Keine {{name}} vorhanden"
}
```

Templates pass values as hash parameters: `{{t "page_of" page=pagination.page total=pagination.total_pages}}`.

Built-in validation messages (the `validation.*` keys) receive the field as
`{{field}}`. In the admin that is the field's label in the viewer's UI locale —
for a field inside a group, array or blocks row, prefixed by its containers'
labels (`SEO › Title`). A field without a label shows its name title-cased.
The API surfaces (gRPC, MCP, Lua) return the English `message`, which names the
field by its schema name.

### System email subjects

The subject lines of the emails the CMS sends itself are translation keys too:

| Key | English |
| --- | --- |
| `email.subject.verify_email` | Verify your email |
| `email.subject.password_reset` | Reset your password |
| `email.subject.mfa_code` | Your verification code |

English and German are built in. Each email's subject is written in the
recipient's admin UI language (the language they picked in the admin header)
and, for a user who never picked one, in `default_locale`; a locale without the
key falls back to English. Override a subject — or add one for another
language — in `<config_dir>/translations/<locale>.json` like any other string.
A subject must be a single line: an override containing a line break fails the
start (`serve` and `work`) with an error naming the key and locale. The email
bodies are the `templates/email/*.hbs` templates, overridable from
`<config_dir>/templates/email/`.

### Available Keys

See `translations/en.json` in the source tree for all available translation keys.

## Backward Compatibility

- No `[locale]` config or empty `locales` = feature completely disabled
- No `localized = true` on fields = no locale columns created
- All existing behavior is preserved when localization is not configured
- Plain string labels/descriptions/placeholders work exactly as before
