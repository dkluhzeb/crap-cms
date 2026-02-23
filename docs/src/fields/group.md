# Group

Visual grouping of sub-fields. Sub-fields become prefixed columns on the parent table (no join table).

## Storage

Group fields do **not** create their own column. Instead, each sub-field becomes a column with a double-underscore prefix: `{group}__{sub}`.

For example, a group named `seo` with sub-fields `title` and `description` creates columns:
- `seo__title TEXT`
- `seo__description TEXT`

## Definition

```lua
{
    name = "seo",
    type = "group",
    fields = {
        { name = "title", type = "text", required = true },
        { name = "description", type = "textarea" },
        { name = "no_index", type = "checkbox" },
    },
    admin = {
        description = "Search engine optimization settings",
    },
}
```

## Sub-Fields

Sub-fields support the same properties as regular fields (name, type, required, default_value, admin, etc.) but do not support nested groups, arrays, or relationships.

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

## Admin Rendering

Renders as a `<fieldset>` with a legend. Each sub-field is rendered using its own field type template (text, checkbox, select, etc.).
