# Rich Text

Rich text field with a ProseMirror-based WYSIWYG editor. Stored as HTML (default) or
ProseMirror JSON.

## SQLite Storage

`TEXT` column containing HTML content (default) or ProseMirror JSON document.

## Definition

```lua
crap.fields.richtext({
    name = "content",
    admin = {
        placeholder = "Write your content...",
    },
})
```

## Storage Format

By default, richtext fields store raw HTML. Set `admin.format = "json"` to store the
ProseMirror document structure as JSON instead:

```lua
crap.fields.richtext({
    name = "content",
    admin = {
        format = "json",
    },
})
```

### HTML vs JSON

| | HTML (default) | JSON |
|---|---|---|
| Storage | Raw HTML string | ProseMirror `doc.toJSON()` |
| Round-trip fidelity | Loses some structural info | Lossless |
| Programmatic manipulation | Parse HTML | Walk JSON tree |
| FTS search | Indexed as-is | Plain text extracted automatically |
| API response | HTML string | JSON string |

### Important notes

- **Changing format does NOT migrate existing data.** If you switch from `"html"` to
  `"json"` (or vice versa), existing documents retain their original format. The editor
  will attempt to parse the stored content according to the current format setting.
- The API returns the stored format as-is (HTML string or JSON string).
- Full-text search automatically extracts plain text from JSON-format richtext fields.

## Toolbar Configuration

By default, all toolbar features are enabled. Use `admin.features` to limit which
features are available:

```lua
crap.fields.richtext({
    name = "content",
    admin = {
        features = { "bold", "italic", "heading", "link", "bulletList" },
    },
})
```

### Available Features

| Feature | Description |
|---|---|
| `bold` | Bold text (Ctrl+B) |
| `italic` | Italic text (Ctrl+I) |
| `code` | Inline code (Ctrl+\`) |
| `link` | Hyperlinks |
| `heading` | H1, H2, H3 headings |
| `blockquote` | Block quotes |
| `orderedList` | Numbered lists |
| `bulletList` | Bullet lists |
| `codeBlock` | Code blocks (```) |
| `horizontalRule` | Horizontal rule |

When `features` is omitted or empty, all features are enabled (backward compatible).
Undo/redo buttons are always available regardless of feature configuration.

## Custom Nodes

Custom ProseMirror nodes let you embed structured components (CTAs, embeds, alerts,
mentions, etc.) inside richtext content. Register nodes in `init.lua`, then enable
them on specific fields via `admin.nodes`.

### Registration

Node attributes use the same `crap.fields.*` factory functions as collection fields.
Only scalar types are allowed: `text`, `number`, `textarea`, `select`, `radio`,
`checkbox`, `date`, `email`, `json`, `code`.

```lua
-- init.lua
crap.richtext.register_node("cta", {
    label = "Call to Action",
    inline = false, -- block-level node
    attrs = {
        crap.fields.text({ name = "text", required = true, admin = { label = "Button Text" } }),
        crap.fields.text({ name = "url", required = true, admin = { label = "URL", placeholder = "https://..." } }),
        crap.fields.select({ name = "style", admin = { label = "Style" }, options = {
            { label = "Primary", value = "primary" },
            { label = "Secondary", value = "secondary" },
        }}),
    },
    searchable_attrs = { "text" },
    render = function(attrs)
        return string.format(
            '<a href="%s" class="btn btn--%s">%s</a>',
            attrs.url, attrs.style or "primary", attrs.text
        )
    end,
})
```

### Field configuration

```lua
crap.fields.richtext({
    name = "content",
    admin = {
        format = "json",
        nodes = { "cta" },
        features = { "bold", "italic", "heading", "link", "bulletList" },
    },
})
```

### Node spec options

| Option | Type | Description |
|---|---|---|
| `label` | string | Display label (defaults to node name) |
| `inline` | boolean | Inline vs block-level (default: false) |
| `attrs` | table[] | Attribute definitions (see below) |
| `searchable_attrs` | string[] | Attr names included in FTS search index |
| `render` | function | Server-side render function: `(attrs) -> html` |

### Allowed attribute types

Node attrs support all scalar field types. Complex types (`array`, `group`, `blocks`,
`relationship`, `upload`, `richtext`, `row`, `collapsible`, `tabs`, `join`) are rejected
at registration time.

| Type | Admin Input |
|---|---|
| `text` | Text input |
| `number` | Number input |
| `textarea` | Multi-line textarea |
| `select` | Dropdown with options |
| `radio` | Radio button group |
| `checkbox` | Checkbox |
| `date` | Date picker |
| `email` | Email input |
| `json` | Monospace textarea |
| `code` | Monospace textarea |

### Supported attribute features

Node attrs support most field features that make sense in the richtext context.

#### Admin display hints

These control how attributes appear in the node edit modal:

| Feature | Effect |
|---|---|
| `admin.hidden` | Attribute is not rendered in the modal (value preserved) |
| `admin.readonly` | Input is read-only / disabled |
| `admin.width` | CSS width on the field container (e.g. `"50%"`) |
| `admin.step` | `step` attribute on number inputs (e.g. `"0.01"`) |
| `admin.rows` | Number of rows for textarea/code/json fields |
| `admin.language` | Language label suffix for code fields (e.g. `"JSON"`) |
| `admin.placeholder` | Placeholder text on inputs |
| `admin.description` | Help text below the input |
| `min` / `max` | Min/max on number inputs |
| `min_length` / `max_length` | Minlength/maxlength on text/textarea inputs |
| `min_date` / `max_date` | Min/max on date inputs |
| `picker_appearance` | Date input type: `"dayOnly"` (default), `"dayAndTime"`, `"timeOnly"`, `"monthOnly"` |

#### Server-side validation

Node attribute values inside richtext content are validated server-side on create/update.
The following checks run automatically:

| Check | Description |
|---|---|
| `required` | Attribute must have a non-empty value |
| `validate` | Custom Lua validation function |
| `min_length` / `max_length` | Text length bounds |
| `min` / `max` | Numeric bounds |
| `min_date` / `max_date` | Date bounds |
| email format | Valid email for `email` type attrs |
| option validity | Value must be in `options` for `select`/`radio` |

Validation errors reference the node location: `"content[cta#0].url"` (first CTA node's
`url` attribute in the `content` field).

#### `before_validate` hooks

Node attrs support `hooks.before_validate` for normalizing values before validation:

```lua
crap.richtext.register_node("cta", {
    label = "CTA",
    attrs = {
        crap.fields.text({
            name = "url",
            required = true,
            hooks = {
                before_validate = { "hooks.trim_whitespace" },
            },
        }),
    },
})
```

The hook receives `(value, context)` and returns the transformed value. Runs before
validation checks.

#### Unsupported features

These features have no effect on node attributes and will produce a warning at
registration time:

| Feature | Reason |
|---|---|
| `hooks.before_change` | No per-attr write lifecycle |
| `hooks.after_change` | No per-attr write lifecycle |
| `hooks.after_read` | No per-attr read lifecycle |
| `access` (read/create/update) | No per-attr access control |
| `unique` | No DB column |
| `index` | No DB column |
| `localized` | Richtext field itself is localized or not |
| `mcp.description` | Not exposed as MCP fields |
| `has_many` | Doesn't apply to scalar node attrs |
| `admin.condition` | Not yet supported (deferred) |

### Server-side rendering

Use `crap.richtext.render(content)` in hooks to replace custom nodes with rendered
HTML. The function auto-detects format (JSON or HTML). Custom nodes with a `render`
function produce the function's output; nodes without one pass through as
`<crap-node>` custom elements.

```lua
-- In an after_read hook
function hooks.render_content(context)
    local doc = context.doc
    if doc.content then
        doc.content = crap.richtext.render(doc.content)
    end
    return context
end
```

### FTS search

Custom node attributes listed in `searchable_attrs` are automatically extracted
for full-text search when the field uses JSON format.

## Resize Behavior

By default, the richtext editor is vertically resizable (no max-height constraint). Set
`admin.resizable = false` to lock it to a fixed height range (200–600px):

```lua
crap.fields.richtext({
    name = "content",
    admin = {
        resizable = false,
    },
})
```

## Admin Rendering

Renders as a ProseMirror-based rich text editor with a configurable toolbar. When
custom nodes are configured, an insert button group appears in the toolbar for each
node type. Nodes display as styled cards (block) or pills (inline) in the editor;
double-click to edit attributes.

## Notes

- No server-side sanitization is applied — sanitize in hooks if needed
- The toolbar configuration only affects the admin UI; it does not validate or strip content server-side
- Custom node names must be alphanumeric with underscores only
