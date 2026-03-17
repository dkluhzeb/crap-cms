# crap.richtext

Register custom ProseMirror node types for the rich text editor and render rich text content to HTML.

## Functions

### `crap.richtext.register_node(name, spec)`

Register a custom rich text node type.

**Parameters:**
- `name` (string) — Node name (alphanumeric + underscores only).
- `spec` (table) — Node specification.

**Spec fields:**

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `label` | string | `name` | Display label in the editor toolbar |
| `inline` | boolean | `false` | Whether this is an inline node (vs block) |
| `attrs` | table[] | `{}` | Node attributes (name, type, options) |
| `searchable_attrs` | string[] | `{}` | Attribute names included in full-text search |
| `render` | function | `nil` | Custom HTML render function `(attrs) -> string` |

```lua
crap.richtext.register_node("callout", {
    label = "Callout",
    attrs = {
        { name = "type", type = "select", options = {
            { label = "Info", value = "info" },
            { label = "Warning", value = "warning" },
        }},
        { name = "body", type = "text" },
    },
    searchable_attrs = { "body" },
    render = function(attrs)
        return string.format(
            '<div class="callout callout-%s">%s</div>',
            attrs.type or "info",
            attrs.body or ""
        )
    end,
})
```

### `crap.richtext.render(content)`

Render a rich text JSON string to HTML, including any registered custom nodes.

**Parameters:**
- `content` (string) — ProseMirror JSON content string.

**Returns:** string — Rendered HTML.

```lua
local html = crap.richtext.render(doc.body)
```

## Notes

- Register nodes in `init.lua` so they're available to all VMs.
- Custom nodes appear in the rich text editor toolbar for fields that include them.
- The `render` function is called during `crap.richtext.render()` to convert custom nodes to HTML.
