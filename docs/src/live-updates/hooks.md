# Live Update Hooks

## `before_broadcast`

A lifecycle event that fires after the write transaction has committed, before the event reaches the EventBus. Hooks can suppress events or transform the broadcast data.

### Collection-Level

```lua
crap.collections.define("posts", {
    hooks = {
        before_broadcast = { "hooks.posts.filter_broadcast" },
    },
})
```

The hook function receives `{ collection, operation, data }` and returns:

- The context table (possibly with modified `data`) to continue broadcasting
- `false` or `nil` to suppress the event entirely

```lua
-- hooks/posts.lua
local M = {}

function M.filter_broadcast(ctx)
    if ctx.operation == "delete" then return ctx end
    if ctx.data.status == "published" then
        return ctx  -- broadcast
    end
    return false  -- suppress draft changes
end

return M
```

### Registered Hooks

Global registered hooks also fire for `before_broadcast`:

```lua
-- init.lua
crap.hooks.register("before_broadcast", function(ctx)
    -- Strip sensitive fields from all broadcast data
    ctx.data._password_hash = nil
    ctx.data._reset_token = nil
    return ctx
end)
```

### Execution Order

1. Collection-level `before_broadcast` hooks (string refs from definition)
2. Global registered `before_broadcast` hooks (`crap.hooks.register`)

If any hook returns `false`/`nil`, the event is suppressed and no further hooks run.

### CRUD Access

`before_broadcast` hooks run after the transaction has committed and do **not** have CRUD access.

## `live` Setting Functions

When `live` is a string (Lua function reference), the function is called before `before_broadcast` hooks:

```lua
crap.collections.define("posts", {
    live = "hooks.posts.should_broadcast",
})
```

```lua
function M.should_broadcast(ctx)
    -- Only broadcast published posts
    return ctx.data.status == "published"
end
```

The function receives `{ collection, operation, data }` and returns `true`/`false`. This is a fast gate — `before_broadcast` hooks only run if the `live` check passes.
