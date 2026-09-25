# crap.richtext

Register custom ProseMirror node types for the rich text editor and render rich text content to HTML.

## Functions

### `crap.richtext.register_node(name, spec)`

Register a custom rich text node type. **Init-only:** call from
`init.lua` or any file loaded by `require` from it. Runtime calls
error because the pool of Lua VMs each holds its own node table —
registering at runtime would only land in the VM that ran the call,
fragmenting the set.

**Parameters:**
- `name` (string) — Node name. Must be a valid slug: lowercase ASCII
  letters, digits, and underscores, not starting with a digit or
  underscore (e.g. `callout`, `pull_quote`) — same rule as collection and
  job slugs. Must not collide with a built-in ProseMirror node type.
- `spec` (table) — Node specification. Unknown keys are rejected;
  `inline` must be a boolean (a wrong-typed value errors at load).

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

Settings that have no effect on node attrs (`unique`, `index`, `localized`,
`required_locales`, `has_many`, `required_when`, `access`,
`hooks.before_change/after_change/after_read`, `mcp.description`, `admin.condition`) are a
registration error naming the node, the attr and the settings. `hooks.before_validate`
hooks get the field hook context and fail closed like field-level hooks (see
[Rich Text — `before_validate` hooks](../fields/richtext.md#before_validate-hooks)).

The init VM and every pool VM run the same checks on a registration, so a spec is either
accepted everywhere or refused at load.

```lua
local function escape_html(s)
    return (tostring(s or ""):gsub("&", "&amp;"):gsub("<", "&lt;"):gsub(">", "&gt;")
        :gsub('"', "&quot;"):gsub("'", "&#39;"))
end

crap.richtext.register_node("callout", {
    label = "Callout",
    attrs = {
        crap.fields.select({ name = "type", options = {
            { label = "Info", value = "info" },
            { label = "Warning", value = "warning" },
        }}),
        crap.fields.textarea({ name = "body", admin = { rows = 4 } }),
    },
    searchable_attrs = { "body" },
    render = function(attrs)
        -- escape_html: see "Render output is NOT sanitized" below.
        return string.format(
            '<div class="callout callout-%s">%s</div>',
            escape_html(attrs.type or "info"),
            escape_html(attrs.body)
        )
    end,
})
```

### `crap.richtext.render(content, opts)`

Render rich text to HTML, replacing registered custom nodes with the output of their
`render` functions (nodes without one pass through as `<crap-node>` elements).

**Parameters:**
- `content` (string | table | nil) — a rich text field's value as read: the document
  table of an `admin.format = "json"` field, or the string of either format (HTML, or
  ProseMirror JSON text). `nil` renders as `""`.
- `opts` (table, optional) — `format` (`"html"` or `"json"`): the string's storage
  format, i.e. the field's `admin.format`. Unknown keys are rejected.

A table is always a JSON document (`{ type = "doc", ... }`; any other table is an
error). A string's format is `opts.format` when given; otherwise the string is JSON only
when it holds a document object (`"type": "doc"`), and HTML otherwise — so HTML or plain
text that starts with `{` renders as HTML instead of raising. With `format = "json"`, a
string that is not valid JSON raises a render error.

In a JSON document, a link whose URL is neither relative nor `http`/`https`/`mailto`/`tel`
renders as `href="#"`. HTML content is not sanitized.

**Returns:** string — Rendered HTML.

```lua
-- A JSON-format field reads as a document table:
local html = crap.richtext.render(context.data.body)

-- State the format when rendering a stored string:
local html = crap.richtext.render(raw_value, { format = "html" })
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
