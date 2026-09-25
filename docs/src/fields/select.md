# Select

Single-value selection from predefined options.

## SQLite Storage

`TEXT` column storing the selected `value`.

## Definition

```lua
crap.fields.select({
    name = "status",
    required = true,
    default_value = "draft",
    options = {
        { label = "Draft", value = "draft" },
        { label = "Published", value = "published" },
        { label = "Archived", value = "archived" },
    },
})
```

## Options Format

Each option is a table with:

| Property | Type | Description |
|----------|------|-------------|
| `label` | string \| table | Display text in the admin UI. Supports [localized strings](../locale/overview.md#admin-label-localization) (`{ en = "Red", de = "Rot" }`). |
| `value` | string | Stored value in the database |

## Multi-Value (`has_many`)

Allow selecting multiple options. Values are stored as a JSON array in a TEXT column.

```lua
crap.fields.select({
    name = "categories",
    has_many = true,
    options = {
        { label = "News", value = "news" },
        { label = "Tech", value = "tech" },
        { label = "Sports", value = "sports" },
    },
})
```

Filters match element by element — `{ categories = "news" }` finds documents
with `news` among their selections, `{ categories = { not_equals = "news" } }`
those without it; see
[Query & Filters](../query-and-filters/overview.md#has-many-fields-element-by-element).
A has-many select cannot be a sort key.

## Removing an Option

A value must be one of the declared options — except a value the document
already holds. When an option is removed, documents that store it keep it: the
edit form shows it as a marked, selected option, and an update that resubmits it
unchanged passes validation (on its own, or as one element of a `has_many`
list). This holds at every depth — a top-level field, a group, an array or
blocks row, and anything nested inside a row — and for a select attr of a custom
rich text node. The document's pending draft counts as held too, for a
collection with drafts. A value the document does not already hold is refused
(`validation.invalid_option` / `validation.invalid_option_value`), so a removed
option can never be newly chosen, and a create has nothing held.

## Admin Rendering

Renders as a `<select>` dropdown. When `has_many = true`, renders as a multi-select.
