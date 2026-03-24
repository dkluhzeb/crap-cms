# Auth Collections

Any collection can be an auth collection. Set `auth = true` for defaults, or provide a configuration table.

## Simple Auth

```lua
crap.collections.define("users", {
    auth = true,
    -- ...
})
```

## Auth Config Table

```lua
crap.collections.define("users", {
    auth = {
        token_expiry = 3600,       -- 1 hour (default: 7200 = 2 hours)
        disable_local = false,      -- set true to disable password login
        strategies = {
            {
                name = "api-key",
                authenticate = "hooks.auth.api_key_check",
            },
        },
    },
    -- ...
})
```

## Config Properties

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `token_expiry` | integer | `7200` | JWT token lifetime in seconds. Overrides the global `[auth] token_expiry`. |
| `disable_local` | boolean | `false` | When `true`, the password login form is hidden. Only custom strategies can authenticate. |
| `verify_email` | boolean | `false` | When `true`, new users must verify their email before logging in. Requires email configuration. |
| `forgot_password` | boolean | `true` | When `true`, enables the "Forgot password?" flow. Requires email configuration. |
| `strategies` | AuthStrategy[] | `{}` | Custom auth strategies. See [Custom Strategies](custom-strategies.md). |

## Email Auto-Injection

When `auth = true` and no `email` field exists in the field definitions, one is automatically injected:

```lua
{
    name = "email",
    type = "email",
    required = true,
    unique = true,
    admin = { placeholder = "user@example.com" },
}
```

If you define your own `email` field, it's used as-is.

## Password Storage

Auth collections get a hidden `_password_hash` TEXT column during schema migration. This column:

- Is **not** a regular field — it doesn't appear in `def.fields`
- Is **never** returned in API responses
- Is **never** included in hook contexts
- Is **never** shown in admin forms
- Is only accessed by dedicated auth functions (`update_password`, `get_password_hash`)

## Password Policy

All password-setting paths (create, update, reset, CLI) enforce the password policy configured in `[auth.password_policy]` in `crap.toml`. Defaults: min 8 characters, max 128 characters. See [crap.toml reference](../configuration/crap-toml.md#authpassword_policy) for all options.

## Password in Create/Update

When creating or updating a user, the `password` field (if present in the data) is:

1. Extracted from the data before hooks run
2. Hashed with Argon2id after the document is written
3. Stored in the `_password_hash` column

In the admin UI:
- **Create form** — password is required
- **Edit form** — password is optional ("leave blank to keep current")

## JWT Claims

Tokens contain:

| Claim | Description |
|-------|-------------|
| `sub` | User document ID |
| `collection` | Auth collection slug (e.g., "users") |
| `email` | User email |
| `exp` | Expiration timestamp (Unix) |
| `iat` | Issued-at timestamp (Unix) |
