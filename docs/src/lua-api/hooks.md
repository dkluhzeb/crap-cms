# crap.hooks

Global hook registration API. Register hooks in `init.lua` to fire for all collections.

## crap.hooks.register(event, fn)

Register a hook function for a lifecycle event.

```lua
crap.hooks.register("before_change", function(ctx)
    crap.log.info("[audit] " .. ctx.operation .. " on " .. ctx.collection)
    return ctx
end)
```

### Parameters

| Parameter | Type | Description |
|-----------|------|-------------|
| `event` | string | Lifecycle event name |
| `fn` | function | Hook function receiving a context table |

### Events

| Event | Description |
|-------|-------------|
| `before_validate` | Before field validation on create/update |
| `before_change` | After validation, before write on create/update |
| `after_change` | After create/update (runs in transaction, has CRUD access) |
| `before_read` | Before returning read results |
| `after_read` | After read, before response (no CRUD access) |
| `before_delete` | Before delete |
| `after_delete` | After delete (runs in transaction, has CRUD access) |
| `before_broadcast` | Before live event broadcast (can suppress or transform) |
| `before_render` | Before rendering admin pages (receives full template context, can modify it; global-only, no CRUD access) |

## crap.hooks.remove(event, fn)

Remove a previously registered hook. Uses `rawequal` for identity matching — you must pass the exact same function reference.

```lua
local my_hook = function(ctx) return ctx end

crap.hooks.register("before_change", my_hook)
crap.hooks.remove("before_change", my_hook)
```

### Parameters

| Parameter | Type | Description |
|-----------|------|-------------|
| `event` | string | Lifecycle event name |
| `fn` | function | The exact function reference to remove |

## crap.hooks.list(event)

Return the list of registered hook functions for an event. Useful for debugging or introspection.

```lua
local hooks = crap.hooks.list("before_change")
print(#hooks)  -- number of registered before_change hooks
```

### Parameters

| Parameter | Type | Description |
|-----------|------|-------------|
| `event` | string | Lifecycle event name |

### Returns

A Lua table (array) of the registered hook functions. Empty table if none are registered.
