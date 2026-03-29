# Access Control

Crap CMS provides opt-in access control at both collection and field levels. Access functions are Lua function refs that return one of three values:

- `true` — allowed
- `false` or `nil` — denied
- A filter table (read only) — allowed with query constraints

## Opt-In

If no access control is configured, everything is allowed. This is fully backward compatible with existing setups.

To enforce a "secure by default" posture, set `default_deny = true` in `[access]` in `crap.toml`. With this setting, collections and globals without explicit access functions **deny all operations** instead of allowing them. Every collection must then explicitly declare its access rules.

## Three Levels

1. **Admin panel-level** — `admin.access` in `crap.toml`. A Lua function that gates access to the entire admin UI, checked after login. See [Admin UI](../admin-ui/overview.md#access).
2. **Collection-level** — controls who can read, create, update, or delete documents in a collection. See [Collection-Level](collection-level.md).
3. **Field-level** — controls which fields are visible or writable per-user. See [Field-Level](field-level.md).

## Access Function Context

All access functions receive a context table:

```lua
function M.check(ctx)
    -- ctx.user  = full user document (or nil if anonymous)
    -- ctx.id    = document ID (for update/delete/find_by_id)
    -- ctx.data  = incoming data (for create/update)
    return true  -- or false, or a filter table
end
```

| Field | Type | Present When | Description |
|-------|------|-------------|-------------|
| `user` | table or nil | Always | Full user document from the auth collection. `nil` if no auth or anonymous. |
| `id` | string or nil | update, delete, find_by_id | Document ID |
| `data` | table or nil | create, update | Incoming data |

## CRUD Access in Access Functions

Access functions run with transaction context — they can call `crap.collections.find()` etc. to make decisions based on data in other collections.

> **Note:** Lua CRUD functions enforce access control by default (`overrideAccess = false`). If your access function calls CRUD internally, pass `overrideAccess = true` to avoid recursive access checks:
>
> ```lua
> function M.check(ctx)
>     local count = crap.collections.count("items", { overrideAccess = true })
>     return count < 100  -- allow if under limit
> end
> ```
