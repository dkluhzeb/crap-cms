# Row

Layout-only grouping of sub-fields. Unlike [Group](group.md), sub-fields are promoted as top-level columns with no prefix.

## Storage

Row fields do **not** create their own column. Each sub-field becomes a top-level column using its plain name — no prefix is added.

For example, a row with sub-fields `firstname` and `lastname` creates columns:
- `firstname TEXT`
- `lastname TEXT`

This is different from [Group](group.md), which prefixes sub-field columns (`seo__title`).

## Definition

```lua
crap.fields.row({
    name = "name_row",
    fields = {
        crap.fields.text({ name = "firstname", required = true }),
        crap.fields.text({ name = "lastname", required = true }),
    },
})
```

## API Representation

In API responses, row sub-fields appear as flat top-level fields (not nested):

```json
{
  "firstname": "Jane",
  "lastname": "Doe"
}
```

## Writing Row Data

Use the plain sub-field names directly — no prefix needed:

```json
{
  "firstname": "Jane",
  "lastname": "Doe"
}
```

## Nesting

Row can be nested inside other layout wrappers (Tabs, Collapsible) and inside Array/Blocks sub-fields at arbitrary depth. All nesting combinations work — see the [Layout Wrappers](overview.md#layout-wrappers) section for details and examples.

> **Depth limit:** The admin UI caps layout nesting at 5 levels. The data layer has no limit.

## Admin Rendering

Sub-fields are rendered in a horizontal row layout. The row itself has no fieldset, legend, or collapsible wrapper — it is purely a layout mechanism for placing related fields side by side.
