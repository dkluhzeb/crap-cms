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
| `attrs` | FieldDefinition[] | `{}` | Node attributes via `crap.fields.*` (scalar types only) |
| `searchable_attrs` | string[] | `{}` | Attribute names included in full-text search |
| `render` | function | `nil` | Custom HTML render function `(attrs) -> string` |

Node attributes use `crap.fields.*` factory functions (same as collection fields).
Only scalar types are allowed: `text`, `number`, `textarea`, `select`, `radio`,
`checkbox`, `date`, `email`, `json`, `code`.

Supported attribute features:

- **Admin display hints:** `admin.hidden`, `admin.readonly`, `admin.width`, `admin.step`,
  `admin.rows`, `admin.language`, `admin.placeholder`, `admin.description`
- **Validation bounds:** `required`, `validate`, `min`/`max`, `min_length`/`max_length`,
  `min_date`/`max_date`, `picker_appearance`
- **Lifecycle hooks:** `hooks.before_validate` (normalize values before validation)

Features that have no effect on node attrs (`unique`, `index`, `localized`, `has_many`,
`access`, `hooks.before_change/after_change/after_read`, `mcp`, `admin.condition`) produce
a warning at registration time but do not error.

```lua
crap.richtext.register_node("callout", {
    label = "Callout",
    attrs = {
        crap.fields.select({ name = "type", options = {
            { label = "Info", value = "info" },
            { label = "Warning", value = "warning" },
        }}),
        crap.fields.text({ name = "body", admin = { rows = 4 } }),
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

## Render output is NOT sanitized

The string your `render` function returns is inserted directly into the HTML output —
**no escaping, no sanitization, no tag allowlist**. Whatever you return becomes
raw markup in the rendered page.

This is by design: custom nodes exist precisely so operators can emit structured
HTML. But the trust boundary is strict:

- `render` is **trusted code**. It runs server-side inside Lua you wrote.
- Any user-supplied data you interpolate into the output string (node attrs,
  document content, etc.) **must** be escaped by you before concatenation.

Safe pattern — escape the parts that came from user input:

```lua
local function escape_html(s)
    s = s or ""
    s = s:gsub("&", "&amp;"):gsub("<", "&lt;"):gsub(">", "&gt;")
    s = s:gsub('"', "&quot;"):gsub("'", "&#39;")
    return s
end

crap.richtext.register_node("callout", {
    attrs = { crap.fields.text({ name = "body" }) },
    render = function(attrs)
        return '<div class="callout">' .. escape_html(attrs.body) .. '</div>'
    end,
})
```

Unsafe pattern — concatenating a user field directly produces stored XSS:

```lua
render = function(attrs)
    return '<div class="callout">' .. (attrs.body or "") .. '</div>'  -- BAD
end
```

Server-side richtext output is NOT passed through any sanitizer — there is no
fallback. Treat every interpolation as a potential injection vector.
