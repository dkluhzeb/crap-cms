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
| FTS search | Plain text extracted automatically | Plain text extracted automatically |
| API response | HTML string | parsed JSON document |

### Important notes

- **Changing format does NOT migrate existing data.** If you switch from `"html"` to
  `"json"` (or vice versa), existing documents retain their original format. Migrate
  such values after switching: a value that is still HTML is not a JSON document, so the
  admin editor shows it read-only with an error (see [Content the editor cannot
  open](#content-the-editor-cannot-open)). Saving the document unchanged keeps it; any
  write that changes it to another non-document fails validation with
  `validation.invalid_richtext_json`.
- The API returns an HTML string for `"html"` and the parsed document (a table/object, not a string) for `"json"`.
- Full-text search indexes the plain text of both formats (see [FTS search](#fts-search)).

### Validation

Every write — admin, gRPC, MCP, Lua CRUD, at any depth (groups, array/blocks rows) —
checks the value's shape before storing it:

- **`"html"`**: the value must be a string (`validation.invalid_richtext_html`).
- **`"json"`**: the value must be a ProseMirror document the field's editor can open —
  sent as its JSON text or as the document object itself. The root is
  `{ "type": "doc", "content": [...] }`; every node and mark type must be one the field
  enables (see [Available Features](#available-features) and [Custom
  Nodes](#custom-nodes)); attributes must be ones the type declares; text nodes carry
  non-empty text. A string, number, list or anything else that is not a document fails
  with `validation.invalid_richtext_json`; a node or mark the field does not enable
  fails with `validation.richtext_node_not_allowed` / `validation.richtext_mark_not_allowed`.

**A value the document already holds is accepted unchanged.** On an update, a value
that fails the check above but is identical to the one the document stores in that
field — or its pending draft, for a collection with drafts — passes: disabling a
feature must not make every later save of a document written with it fail. This holds
at every depth; inside array and blocks rows the value counts as held when any row of
the same field holds it, so reordering rows keeps it. A value the document does not
already hold is refused, so content the field no longer allows is never newly written.
As for a removed select option, a value counts as held only where the writer may read
it — its field's `access.read` / `hidden`, and the draft view for the pending draft (see
[Removing an Option](select.md#removing-an-option)).
A version restore brings back a snapshot's values, so one the live document no longer
holds is judged like any other new value.

The always-available nodes are `paragraph`, `text` and `hard_break`. The rest map to
features:

| Feature | Node / mark type in the document |
|---|---|
| `bold` | mark `strong` |
| `italic` | mark `em` |
| `code` | mark `code` |
| `link` | mark `link` (attrs `href`, `title`, `target`, `rel`) |
| `heading` | node `heading` (attr `level`, a number) |
| `blockquote` | node `blockquote` |
| `orderedList` / `bulletList` | nodes `ordered_list` (attr `order`, a number), `bullet_list`, `list_item` — either feature enables all three |
| `codeBlock` | node `code_block` |
| `horizontalRule` | node `horizontal_rule` |

**`required`** rejects a blank value, not only an empty one: an editor emptied of its
text still submits markup (`<p></p>`, or a document holding one empty paragraph). A
value is blank when it has no visible text and no custom node — in both formats, and
for `required_locales` completeness too.

**`min_length` / `max_length`** measure the plain text (markup is not counted), in both
formats and for a `"json"` value sent as text or as an object.

### Content the editor cannot open

When a stored `"json"` value cannot be opened by the field's editor — it holds a node or
mark the field no longer enables (a feature removed from `admin.features` after content
was written), or it is not a document at all — the admin shows the field read-only with
an error and the stored value, instead of an empty editor. The value cannot be edited
there; the form resubmits it exactly as stored, and validation accepts it unchanged (see
[Validation](#validation)), so saving the rest of the document keeps it — at the top
level and inside array and blocks rows alike. Re-enable the feature, or migrate the
value, to edit it again.

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
An unknown feature name is a load error. Undo/redo buttons are always available
regardless of feature configuration.

`format`, `features` and `nodes` apply only to richtext fields; setting them on any
other field type is a load error.

## Custom Nodes

Custom ProseMirror nodes let you embed structured components (CTAs, embeds, alerts,
mentions, etc.) inside richtext content. Register nodes in `init.lua`, then enable
them on specific fields via `admin.nodes`. Every name in `admin.nodes` must be a
registered node — an unregistered name (a typo, a registration file that is never
loaded) fails to boot.

### Registration

Node attributes use the same `crap.fields.*` factory functions as collection fields.
Only scalar types are allowed: `text`, `number`, `textarea`, `select`, `radio`,
`checkbox`, `date`, `email`, `json`, `code`.

```lua
-- init.lua
local function escape_html(s)
    return (tostring(s or ""):gsub("&", "&amp;"):gsub("<", "&lt;"):gsub(">", "&gt;")
        :gsub('"', "&quot;"):gsub("'", "&#39;"))
end

-- Only relative links and http(s)/mailto/tel URLs; anything else (javascript:, data:, …)
-- becomes "#".
local function safe_url(url)
    -- Browsers ignore whitespace and control characters in a scheme
    -- ("java\tscript:"), so drop them before reading it.
    url = tostring(url or ""):gsub("[%c%s]", "")
    local scheme = url:match("^([%a][%w+.-]*):")
    if scheme and not ({ http = true, https = true, mailto = true, tel = true })[scheme:lower()] then
        return "#"
    end
    return url
end

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
        -- Every attr is user input: escape it (see "Render output is NOT sanitized"
        -- in the crap.richtext API reference).
        return string.format(
            '<a href="%s" class="btn btn--%s">%s</a>',
            escape_html(safe_url(attrs.url)),
            escape_html(attrs.style or "primary"),
            escape_html(attrs.text)
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
| `admin.width` | Width in the node modal: `"half"`, `"third"` or a CSS width (e.g. `"50%"`); narrower attrs share a row |
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
| option validity | Value must be in `options` for `select`/`radio` — or one the document already holds on the same attr of a node of the same type in this field (see [Removing an Option](select.md#removing-an-option)) |

Validation errors reference the node location: `"content[cta#0].url"` (first CTA node's
`url` attribute in the `content` field).

The checks run on every write surface (admin, gRPC, MCP, Lua CRUD) and for rich text
fields at any depth — inside groups and array/blocks rows too. A `"json"` field's value
may be sent as the document's JSON text or as the document object itself; a value that
is not a document the field accepts is refused by the field's own [validation](#validation)
(nesting deeper than 127 levels counts as unreadable). In `"html"` content, nodes are
found the way the browser parses the markup: the tag name and attribute names are
case-insensitive, attribute values may use either quote or none, and entity-encoded
attribute values are decoded.

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
validation checks, for rich text fields at any depth (including groups and array/blocks
rows), whether a `"json"` value arrives as text or as a document object.

The context is the [field hook context](../hooks/field-hooks.md#context-table):
`field_name` is the attr's name, `data` the node's attrs, `document` the whole document
being written, plus `collection`, `operation`, `id`, `locale`, `user`, `ui_locale` and
`options`. Like a field-level `before_validate` hook it fails closed: a hook ref that does
not resolve fails to boot, and a hook that raises or returns a value that cannot be
converted fails the write.

#### Unsupported features

These settings have no effect on node attributes, so registering a node whose attrs use
them is a load error:

| Feature | Reason |
|---|---|
| `hooks.before_change` | No per-attr write lifecycle |
| `hooks.after_change` | No per-attr write lifecycle |
| `hooks.after_read` | No per-attr read lifecycle |
| `access` (read/create/update) | No per-attr access control |
| `unique` | No DB column |
| `index` | No DB column |
| `localized` | Richtext field itself is localized or not |
| `required_locales` | Richtext field itself is localized or not |
| `required_when` | Node attrs support static `required` only |
| `mcp.description` | Not exposed as MCP fields |
| `has_many` | The node editor holds one value per attr |
| `admin.condition` | No per-attr display conditions in the node editor |

### Server-side rendering

Use `crap.richtext.render(value, opts?)` in hooks to render a field's value to HTML,
replacing custom nodes with rendered HTML. It takes the value as read: the document
table of a `"json"` field, or the string of either format. Pass `{ format = "json" }` /
`{ format = "html" }` (the field's `admin.format`) to state a string's format; without
it, a string is JSON only when it holds a document object (`"type": "doc"`) and HTML
otherwise. Custom nodes with a `render` function produce the function's output; nodes
without one pass through as `<crap-node>` custom elements. See
[`crap.richtext.render`](../lua-api/richtext.md#craprichtextrendercontent-opts).

```lua
-- A collection after_read hook
function hooks.render_content(context)
    if context.data.content then
        context.data.content = crap.richtext.render(context.data.content)
    end
    return context
end
```

### FTS search

Full-text search indexes the plain text of rich text in both formats: the text content
only — never tag names, attribute values or link targets — with text within a block
kept together (a word split by formatting is still one word) and blocks separated.
Custom node attributes listed in `searchable_attrs` are indexed too, in either format;
other node attrs are not.

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

## Links

A link's URL must be relative (a path, `#fragment`, `?query` or `//host` reference) or
use the `http`, `https`, `mailto` or `tel` scheme. The editor applies this to links
inserted in the link dialog, pasted, and loaded from stored content (a disallowed link
keeps its text but loses the link); `crap.richtext.render` of a `"json"` value renders a
disallowed link as `href="#"`. The scheme is read the way a browser reads it, so
`java&#9;script:` or a leading space does not get past it.

The editor has no image node: pasted images are dropped, and a `"json"` document
containing an `image` node is refused.

## Notes

- No server-side HTML sanitization is applied to `"html"` values — sanitize in hooks if needed
- The toolbar configuration also decides what a `"json"` document may contain: a node or mark of a disabled feature fails validation (see [Validation](#validation))
- Custom node names are lowercase ASCII letters, digits and underscores, not starting with a digit or underscore
