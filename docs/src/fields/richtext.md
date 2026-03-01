# Rich Text

Rich text field stored as an HTML string.

## SQLite Storage

`TEXT` column containing HTML content.

## Definition

```lua
{
    name = "content",
    type = "richtext",
    admin = {
        placeholder = "Write your content...",
    },
}
```

## Toolbar Configuration

By default, all toolbar features are enabled. Use `admin.features` to limit which
features are available:

```lua
{
    name = "content",
    type = "richtext",
    admin = {
        features = { "bold", "italic", "heading", "link", "bulletList" },
    },
}
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

- Content is stored as raw HTML
- No server-side sanitization is applied — sanitize in hooks if needed
- The toolbar configuration only affects the admin UI; it does not validate or strip content server-side
