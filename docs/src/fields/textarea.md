# Textarea

Multi-line text field for longer content.

## SQLite Storage

`TEXT` column.

## Definition

```lua
{
    name = "description",
    type = "textarea",
    admin = {
        placeholder = "Enter a description...",
    },
}
```

## Admin Rendering

Renders as a `<textarea>` element.
