# Code

Code editor field stored as a plain text string. Renders a CodeMirror 6 editor in the admin UI with syntax highlighting and language-aware features.

## SQLite Storage

`TEXT` column containing the raw code string.

## Definition

```lua
{
    name = "metadata",
    type = "code",
    admin = {
        language = "json", -- "json", "javascript", "html", "css", "python", or "plain"
    },
}
```

## Admin Options

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `admin.language` | string | `"json"` | Language mode for syntax highlighting. |

### Supported Languages

| Value | Language |
|-------|----------|
| `json` | JSON |
| `javascript` or `js` | JavaScript |
| `html` | HTML |
| `css` | CSS |
| `python` or `py` | Python |
| `plain` | No syntax highlighting |

## Admin Rendering

Renders as a CodeMirror 6 editor with line numbers, bracket matching, code folding, auto-completion, and search. Falls back to a plain `<textarea>` if the CodeMirror bundle is not loaded.

## Notes

- Content is stored as a raw string (not parsed or validated for syntax)
- Supports `min_length` and `max_length` validation (applied to the raw string)
- The CodeMirror bundle (`static/codemirror.js`) is loaded via `<script defer>` in the admin layout
- To rebuild the bundle: `bash scripts/bundle-codemirror.sh`
