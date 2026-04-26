# Code

Code editor field stored as a plain text string. Renders a CodeMirror 6 editor in the admin UI with syntax highlighting and language-aware features.

## SQLite Storage

`TEXT` column containing the raw code string. When the optional editor-time
language picker is enabled (see below), a companion `<name>_lang` TEXT column
stores the editor's per-document language pick.

## Definition

```lua
crap.fields.code({
    name = "metadata",
    admin = {
        language = "json", -- "json", "javascript", "html", "css", "python", or "plain"
    },
})
```

## Admin Options

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `admin.language` | string | `"json"` | Default language mode for syntax highlighting. |
| `admin.languages` | string[] | `[]` | Optional allow-list. When non-empty, the form renders a language picker next to the editor and persists the choice per document in a `<name>_lang` companion column. |

### Supported Languages

| Value | Language |
|-------|----------|
| `json` | JSON |
| `javascript` or `js` | JavaScript |
| `html` | HTML |
| `css` | CSS |
| `python` or `py` | Python |
| `plain` | No syntax highlighting |

## Per-document language picker

When the document being edited may contain code in different languages
(e.g. an "examples" collection with one block holding JS and the next
holding Python), set `admin.languages` to the allow-list of choices.
The form then renders a small `<select>` next to the editor; changing it
swaps the editor's syntax highlighting and stores the pick in the
companion column so it persists across edits.

```lua
crap.fields.code({
    name = "snippet",
    admin = {
        language  = "javascript",                      -- initial / fallback
        languages = { "javascript", "python", "html" }, -- editor-pickable list
    },
})
```

The companion column is named `<field>_lang` (e.g. `snippet_lang`) and is
created automatically on the next migration. For code fields nested inside
groups, the prefixed naming applies — `meta__snippet` gets a sibling
`meta__snippet_lang`.

When `admin.languages` is empty or absent, the language is fixed to
`admin.language` and no picker, hidden input, or companion column is
emitted.

## Admin Rendering

Renders as a CodeMirror 6 editor with line numbers, bracket matching, code folding, auto-completion, and search. Falls back to a plain `<textarea>` if the CodeMirror bundle is not loaded.

## Notes

- Content is stored as a raw string (not parsed or validated for syntax)
- Supports `min_length` and `max_length` validation (applied to the raw string)
- The CodeMirror bundle (`static/codemirror.js`) is loaded via `<script defer>` in the admin layout
- To rebuild the bundle: `bash scripts/bundle-codemirror.sh`
- The language picker (when enabled) is rendered inside the
  `<crap-code>` shadow root and styled by the same input tokens as the
  rest of the admin (`--input-bg`, `--input-border`, `--text-primary`).
