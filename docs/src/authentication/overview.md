# Authentication

Crap CMS provides built-in authentication via auth-enabled collections. Any collection can serve as an auth collection by setting `auth = true`.

## Key Concepts

- **Auth collection** — a collection with `auth = true`. Users are regular documents in this collection.
- **Two auth surfaces** — Admin UI uses an HttpOnly cookie (`crap_session`). gRPC API uses Bearer tokens.
- **JWT** — all tokens are JWT signed with the configured secret (or an auto-generated one).
- **Argon2id** — passwords are hashed with Argon2id before storage.
- **`_password_hash`** — a hidden column added to auth collection tables. Never exposed in API responses, hooks, or admin forms.
- **Custom strategies** — pluggable auth via Lua functions (API keys, LDAP, SSO).

## Activation

Auth middleware only activates when at least one auth collection exists. If no auth collections are defined, the admin UI and API remain fully open.

## Quick Setup

1. Define an auth collection:

```lua
-- collections/users.lua
crap.collections.define("users", {
    auth = true,
    fields = {
        { name = "name", type = "text", required = true },
        { name = "role", type = "select", options = {
            { label = "Admin", value = "admin" },
            { label = "Editor", value = "editor" },
        }},
    },
})
```

2. Bootstrap the first user:

```bash
cargo run -- --config ./my-project --create-user --email admin@example.com
```

3. Set a JWT secret in production:

```toml
[auth]
secret = "your-random-secret-here"
```
