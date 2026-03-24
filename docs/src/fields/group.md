# Group

Visual grouping of sub-fields. Sub-fields become prefixed columns on the parent table (no join table).

## Storage

Group fields do **not** create their own column. Instead, each sub-field becomes a column with a double-underscore prefix: `{group}__{sub}`.

For example, a group named `seo` with sub-fields `title` and `description` creates columns:
- `seo__title TEXT`
- `seo__description TEXT`

## Definition

```lua
crap.fields.group({
    name = "seo",
    fields = {
        crap.fields.text({ name = "title", required = true }),
        crap.fields.textarea({ name = "description" }),
        crap.fields.checkbox({ name = "no_index" }),
    },
    admin = {
        description = "Search engine optimization settings",
    },
})
```

## Sub-Fields

Sub-fields support the same properties as regular fields (name, type, required, default_value, admin, etc.), including nested groups, arrays, blocks, and relationships.

- **Nested groups** use stacked prefixes: `outer__inner__field`.
- **Arrays/Blocks inside groups** create prefixed join tables: `{collection}_{group}__{field}`.
- **Relationships inside groups** create prefixed junction tables (for has-many) or prefixed columns (for has-one).

## API Representation

In API responses (after hydration), group fields appear as a nested object:

```json
{
  "seo": {
    "title": "My Page Title",
    "description": "A page about...",
    "no_index": 0
  }
}
```

## Writing Group Data

Via gRPC, pass the flat prefixed keys:

```json
{
  "seo__title": "My Page Title",
  "seo__description": "A page about..."
}
```

The double-underscore separator is used in all write operations (forms, gRPC). On read, the prefixed columns are reconstructed into a nested object.

## Filtering on Group Sub-Fields

Use dot notation to filter on group sub-fields. The dot syntax is converted to the double-underscore column name internally.

```lua
crap.collections.find("pages", {
    where = {
        ["seo.title"] = { contains = "SEO" },
        ["seo.no_index"] = "0",
    },
})
```

The equivalent double-underscore syntax also works: `seo__title`.

See [Query & Filters](../query-and-filters/overview.md#nested-field-filters-dot-notation) for filtering on other nested field types (arrays, blocks, relationships).

## Admin Rendering

Renders as a `<fieldset>` with a legend. Each sub-field is rendered using its own field type template (text, checkbox, select, etc.).
