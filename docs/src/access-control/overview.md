# Access Control

Crap CMS provides opt-in access control at both collection and field levels. Access functions are Lua function refs that return one of three values:

- `true` — allowed
- `false` or `nil` — denied
- A filter table (read only) — allowed with query constraints

## Opt-In

If no access control is configured, everything is allowed. This is fully backward compatible with existing setups.

## Two Levels

1. **Collection-level** — controls who can read, create, update, or delete documents in a collection. See [Collection-Level](collection-level.md).
2. **Field-level** — controls which fields are visible or writable per-user. See [Field-Level](field-level.md).

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
