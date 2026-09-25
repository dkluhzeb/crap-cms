# Live Update Hooks

## Execution Order

For every mutation event, the live-update pipeline runs in this strict order — and short-circuits as soon as any step suppresses the event:

1. **`live` setting check** — if `live = false`, the event is dropped immediately. If `live` is a function reference, it is called as a **gate**: returning `false`/`nil` drops the event and prevents `before_broadcast` from running at all.
2. **`before_broadcast` hooks** — collection-level first, then global registered hooks. Any hook returning `false`/`nil` suppresses the event and stops the chain.
3. **EventBus dispatch** — the event is delivered to each matching subscriber, with per-subscriber access checks, `after_read` hooks (full mode only), and field stripping (full mode only). An event that moves a document between content views (a publish or unpublish, a `restore` that changes the status, a soft delete or undelete) reaches a subscriber that could see the document where it was, but not where it is now, as a removal — a `delete` for a collection document, an `update` carrying the empty global for a global leaving the published view (see [Leaving a view](overview.md#access-control)); that subscriber's `after_read` runs with the removal's operation and, for a global, on the empty global. The `live` filter and `before_broadcast` see the event once, with its own operation.

The `live` filter function and `before_broadcast` hooks both receive a context table with `{ collection, operation, data, id, edited_by }` and have similar shapes. `data` is the document as stored — read-shaped (an upload's `sizes` assembled), hidden and read-denied fields included, not stripped for the user who made the change — in `metadata` mode as well as in `full` mode; a delete carries an empty `data`. Both stages run once per event, server-side, before any subscriber's field strip, but they sit at different stages: `live` is the cheap gate, `before_broadcast` is the transformation/suppression stage. `id` is the affected document's id (the serialized event payload sent to subscribers calls the same value `document_id`), and `edited_by` is `{ id, email }` of the user who made the change (`nil` for anonymous changes) — useful for, e.g., suppressing an event for the user who triggered it. `edited_by` exists **only** in these server-side hook contexts; it is never sent to subscribers (the admin SSE payload carries a `self` boolean instead). The `live` filter's context is the typed `crap.LiveFilterContext`; both stages also carry `ctx.options` when the ref was declared as a `{ ref, options }` table (see [Per-Config Options](../hooks/hook-context.md#per-config-options-ctxoptions)).

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

The hook function receives `{ collection, operation, data, id, edited_by }` — `operation` distinguishes the lifecycle mutations (`"undelete"`, `"unpublish"`, `"restore"`) from plain `"update"`s —
(plus `ctx.options` when declared as a `{ ref, options }` table) and returns:

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

### Hook Order Within `before_broadcast`

1. Collection-level `before_broadcast` hooks (string refs from definition)
2. Global registered `before_broadcast` hooks (`crap.hooks.register`)

If any hook returns `false`/`nil`, the event is suppressed and no further hooks run.

### CRUD Access

`before_broadcast` hooks run after the transaction has committed and do **not** have CRUD access.

### Data Modifications Are Event-Only

Mutating `ctx.data` inside a `before_broadcast` hook affects **only** the broadcast event payload — in `full` mode, the document each subscriber's delivery starts from. Every subscriber's payload is still stripped by that subscriber's own field-read access and of hidden fields afterwards, by field name: a field the hook adds is delivered to every subscriber of the view, so never copy a protected value into a new key. In `metadata` mode nothing of `data` is delivered. The stored document is **not** updated — the hook fires post-commit, after the write transaction has already closed. Use this to redact fields, decorate the broadcast with computed values, or flatten internal structure for subscribers, but reach for `before_change` / `after_change` hooks if you actually need the change to land in the database.

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

The function receives `{ collection, operation, data, id, edited_by }` (the typed `crap.LiveFilterContext`) and returns `true`/`false`. This is a fast gate — `before_broadcast` hooks only run if the `live` check passes. See [Execution Order](#execution-order) above.
