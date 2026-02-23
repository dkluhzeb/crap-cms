# Collection-Level Access Control

Collection-level access controls who can perform CRUD operations on a collection.

## Configuration

```lua
crap.collections.define("posts", {
    access = {
        read   = "hooks.access.public_read",
        create = "hooks.access.authenticated",
        update = "hooks.access.authenticated",
        delete = "hooks.access.admin_only",
    },
    -- ...
})
```

Each property is a Lua function ref (string) or `nil` (no restriction).

| Property | Controls |
|----------|----------|
| `read` | `Find` and `FindByID` operations |
| `create` | `Create` operation |
| `update` | `Update` operation |
| `delete` | `Delete` operation |

## Writing Access Functions

Access functions live in Lua modules under the config directory:

```lua
-- hooks/access.lua
local M = {}

-- Allow anyone (including anonymous)
function M.public_read(ctx)
    return true
end

-- Require any authenticated user
function M.authenticated(ctx)
    return ctx.user ~= nil
end

-- Require admin role
function M.admin_only(ctx)
    return ctx.user ~= nil and ctx.user.role == "admin"
end

-- Allow users to only read their own documents
function M.own_only(ctx)
    if ctx.user == nil then return false end
    if ctx.user.role == "admin" then return true end
    return { created_by = ctx.user.id }  -- filter constraint
end

return M
```

## Return Values

| Return Value | Effect |
|-------------|--------|
| `true` | Operation is allowed |
| `false` or `nil` | Operation is denied (403/permission error) |
| table | Read operation is allowed with additional WHERE filters (see [Filter Constraints](filter-constraints.md)) |

Filter table returns are only meaningful for `read` access. For `create`, `update`, and `delete`, a table return is treated as `Allowed`.

## Enforcement Points

- **Admin UI** — middleware checks access before rendering pages
- **gRPC API** — service checks access before executing operations
- Access is checked once, before the operation begins
