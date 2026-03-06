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

## Admin Rendering

Renders as a ProseMirror-based rich text editor with a configurable toolbar.

## Notes

- No server-side sanitization is applied — sanitize in hooks if needed
- The toolbar configuration only affects the admin UI; it does not validate or strip content server-side
