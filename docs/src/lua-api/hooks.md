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
| `after_change` | After create/update (fire-and-forget) |
| `before_read` | Before returning read results |
| `after_read` | After read, before response |
| `before_delete` | Before delete |
| `after_delete` | After delete (fire-and-forget) |

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
