# Field-Level Access Control

Field-level access controls which fields are visible or writable per-user.

## Configuration

```lua
crap.fields.select({
    name = "status",
    access = {
        read = "hooks.access.everyone",
        create = "hooks.access.admin_only",
        update = "hooks.access.admin_only",
    },
    -- ...
})
```

| Property | Controls |
|----------|----------|
| `read` | Whether the field appears in API responses |
| `create` | Whether the field can be set on create |
| `update` | Whether the field can be changed on update |

Omitted properties default to allowed (no restriction).

## How It Works

### Write Access (create/update)

Before a write operation, denied fields are **stripped from the input data**. The operation proceeds with the remaining fields. This means:

- On create: denied fields get their default value (or NULL)
- On update: denied fields keep their current value

### Read Access

After a query, denied fields are **stripped from the response**. The field still exists in the database, but the user doesn't see it.

Fields with `admin.hidden = true` are also stripped from all API responses, regardless of access rules.

## Example

```lua
-- hooks/access.lua
local M = {}

-- Only admins can see the internal_notes field
function M.admin_read(ctx)
    return ctx.user ~= nil and ctx.user.role == "admin"
end

-- Only admins can change the status field
function M.admin_write(ctx)
    return ctx.user ~= nil and ctx.user.role == "admin"
end

return M
```

```lua
-- In collection definition
crap.fields.textarea({
    name = "internal_notes",
    access = {
        read = "hooks.access.admin_read",
    },
}),
crap.fields.select({
    name = "status",
    access = {
        update = "hooks.access.admin_write",
    },
    -- ...
}),
```

## Error Behavior

If a field access function throws an error, the field is treated as **denied** (fail-closed) and a warning is logged.
