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

## Admin Rendering

Renders as a rich text editor in the admin UI.

## Notes

- Content is stored as raw HTML
- No server-side sanitization is applied — sanitize in hooks if needed
