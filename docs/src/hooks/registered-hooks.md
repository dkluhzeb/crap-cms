# Registered Hooks

Registered hooks fire for **all collections** at a given lifecycle event. Register them in `init.lua` using `crap.hooks.register()`.

## Registration

```lua
-- init.lua
crap.hooks.register("before_change", function(ctx)
    crap.log.info("[audit] " .. ctx.operation .. " on " .. ctx.collection)
    return ctx
end)
```

Unlike collection-level hooks (which are string references), registered hooks are Lua functions passed directly.

## API

### `crap.hooks.register(event, fn)`

Register a hook function for a lifecycle event.

| Parameter | Type | Description |
|-----------|------|-------------|
| `event` | string | One of the [lifecycle events](lifecycle-events.md) |
| `fn` | function | Hook function receiving a context table |

### `crap.hooks.remove(event, fn)`

Remove a previously registered hook. Uses `rawequal` for identity-based matching — you must pass the exact same function reference.

```lua
local my_hook = function(ctx)
    -- ...
    return ctx
end

crap.hooks.register("before_change", my_hook)
-- Later:
crap.hooks.remove("before_change", my_hook)
```

## CRUD Access

Registered hooks follow the same rules as collection-level hooks:

- **Before-event hooks** (`before_validate`, `before_change`, `before_delete`) have CRUD access via the shared transaction
- **After-event hooks** (`after_change`, `after_read`, `after_delete`) do NOT have CRUD access

## Execution Order

Registered hooks run **after** field-level and collection-level hooks at each lifecycle stage.

## Example: Audit Log

```lua
-- init.lua
crap.hooks.register("before_change", function(ctx)
    crap.log.info(string.format(
        "[audit] %s %s: %s",
        ctx.operation,
        ctx.collection,
        ctx.data.id or "(new)"
    ))
    return ctx
end)
```

## Example: Auto-Set Created By

```lua
-- init.lua (requires auth + access context in hooks)
crap.hooks.register("before_change", function(ctx)
    if ctx.operation == "create" then
        -- Set created_by to current user ID if field exists
        -- (requires the collection to have a created_by field)
    end
    return ctx
end)
```
