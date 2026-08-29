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

An auth collection declares an ordered **`methods`** list describing how a request can
prove identity. See [Auth Methods](auth-methods.md) for the full method reference.

```lua
crap.collections.define("users", {
    auth = {
        enabled = true,
        token_expiry = 3600,       -- 1 hour (default: 7200 = 2 hours)
        methods = crap.auth.with_defaults({
            -- standard password_login + bearer + session_cookie, plus:
            {
                type = "strategy",
                name = "api-key",
                authenticate = "hooks.auth.api_key_check",
                activates_on = { header = "x-api-key" },
                surfaces = { "grpc" },
            },
        }),
    },
    -- ...
})
```

## Config Properties

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `enabled` | boolean | `true` | Whether auth is active for this collection. Set `false` to disable. |
| `token_expiry` | integer | `7200` | JWT token lifetime in seconds. Overrides the global `[auth] token_expiry`. |
| `methods` | AuthMethod[] | default set | Ordered list of auth methods (`password_login`, `bearer`, `session_cookie`, `strategy`). When `enabled = true` and `methods` is empty, the default set is used. See [Auth Methods](auth-methods.md). |

> The password-only knobs (`mfa`, `mfa_when`, `mfa_deliver`, `verify_email`, `forgot_password`) now live on the
> `password_login` **method** entry, not at the top level. To disable password login,
> omit the `password_login` method instead of the former `disable_local` flag; custom
> authenticators are `strategy` methods rather than a top-level `strategies` list.

## Email Auto-Injection

When `auth = true` and no `email` field exists in the field definitions, one is automatically injected:

```lua
crap.fields.email({
    name = "email",
    required = true,
    unique = true,
    admin = { placeholder = "user@example.com" },
})
```

If you define your own `email` field, it must have `type = "email"` and
`unique = true` — anything else is a load error. The email field is the login
identity, and the case-insensitive uniqueness below is keyed to the email field
*type*, so a `text`-typed or non-unique `email` would allow duplicate accounts.

The email address is matched **case-insensitively** everywhere it identifies an
account: login lookup, the per-account login / forgot-password rate-limit keys,
and the uniqueness check all compare `LOWER(email)`. So `Victim@x.com` and
`victim@x.com` are one account — you cannot register both, and either casing logs
into the same user. The database enforces this too: every auth collection gets a
`UNIQUE INDEX ON (LOWER(email))` (restricted to active rows on soft-delete
collections), so even concurrent registrations can't create case-variant
duplicates. If an existing database already contains such duplicates, migration
fails creating the index — resolve the duplicate accounts first.

## Password Storage

Auth collections get a hidden `_password_hash` TEXT column during schema migration. This column:

- Is **not** a regular field — it doesn't appear in `def.fields`
- Is **never** returned in API responses
- Is **never** included in hook contexts
- Is **never** shown in admin forms
- Is only accessed by dedicated auth functions (`update_password`, `get_password_hash`)

## Password Policy

Every password-setting path enforces the password policy configured in
`[auth.password_policy]` in `crap.toml` — single **and** bulk `create` / `update`
on every surface (Lua, gRPC, MCP, admin), plus password reset and the CLI.
Enforcement lives at the service write chokepoint, so no surface can bypass it: a
policy violation is reported as a `password` field validation error. Defaults: min
8 Unicode characters, max 128 bytes. `min_length` counts Unicode codepoints (so
multi-byte characters count as 1). `max_length` counts bytes (to bound Argon2
hashing cost). See [crap.toml reference](../configuration/crap-toml.md#authpassword_policy)
for all options.

## Password in Create/Update

When creating or updating a user, the `password` field (if present in the data) is:

1. Extracted from the data before hooks run
2. Hashed with Argon2id after the document is written
3. Stored in the `_password_hash` column

In the admin UI:
- **Create form** — password is required
- **Edit form** — password is optional ("leave blank to keep current")

## Account Locking

Auth collections support a `_locked` system field. When a user's `_locked` field is set to a truthy value (e.g., `1`), that user is immediately denied access:

- **JWT validation** — every authenticated request checks `_locked` after resolving the user from the token. A locked user's token is effectively revoked, even if it hasn't expired.
- **`Me` RPC** — returns an `unauthenticated` error for locked users.
- **Admin UI** — the session is rejected and the user is redirected to the login page.

Locking takes effect immediately — no token refresh or logout is needed. Use the CLI to lock/unlock users:

```bash
crap-cms -C ./my-project user lock -e admin@example.com
crap-cms -C ./my-project user unlock -e admin@example.com
```

## JWT Claims

Tokens contain:

| Claim | Description |
|-------|-------------|
| `sub` | User document ID |
| `collection` | Auth collection slug (e.g., "users") |
| `email` | User email |
| `exp` | Expiration timestamp (Unix) |
| `iat` | Issued-at timestamp (Unix) |
