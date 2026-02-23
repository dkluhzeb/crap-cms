# Email

Email address field.

## SQLite Storage

`TEXT` column.

## Definition

```lua
{
    name = "contact_email",
    type = "email",
    required = true,
    unique = true,
    admin = {
        placeholder = "user@example.com",
    },
}
```

## Admin Rendering

Renders as an `<input type="email">` element with browser-native validation.

## Auto-Injection

When a collection has `auth = true` and no `email` field is defined, one is automatically injected with `required = true` and `unique = true`. See [Auth Collections](../authentication/auth-collections.md).
