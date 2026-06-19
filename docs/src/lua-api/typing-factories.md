# crap.any — cross-collection typing factories

`crap.any.*` is the home for **typing-helper factories** that aren't
scoped to a single collection. Each function is a pass-through at
runtime (`f(fn) = fn`); its only purpose is to give LuaLS a typed
parameter slot so `Lua.type.inferParamType` propagates the right
`value` / `ctx` / `data` types into your function body's locals.

For per-collection narrowing (`value` typed per field, `ctx` typed
per collection) use the per-slug accessors on
[`crap.collections.<slug>`](collections.md) and
[`crap.globals.<slug>`](globals.md). Reach for `crap.any.*` when
the hook spans multiple collections or has nothing to narrow on.

## Prerequisites

Both pieces are needed for body-level type inference to fire:

1. **`Lua.type.inferParamType: true`** in your `.luarc.json` (the
   `init` scaffold sets this by default).
2. **Wrap the function literal in a factory call.** A bare
   `return function(ctx) ... end` won't propagate types — LuaLS
   needs the typed parameter slot from the factory's signature.

If your callback's `ctx` shows up as `any` in hover, one of the two
is missing.

## API surface

### `crap.any.collection_hook(fn)`

Wraps a collection lifecycle hook that takes the generic
`crap.HookContext`. Use for hooks that fire on multiple collections
(or for `before_delete` / `after_delete` / `before_broadcast` where
the runtime always passes a generic context).

```lua
-- hooks/set_published_at.lua — used by posts AND projects
return crap.any.collection_hook(function(context)
    -- Publish intent rides `context.draft` (the engine sets the `_status`
    -- column during persist, after this hook runs).
    if not context.draft and not context.data.published_at then
        context.data.published_at = crap.util.date_now()
    end
    return context
end)
```

For collection-specific narrowing, use
[`crap.collections.<slug>.hook(fn)`](collections.md).

### `crap.any.field_hook(fn)`

Wraps a field-level hook that takes `(value, context)` against the
generic `crap.FieldHookContext`. `value` is `any` in the body; for
typed `value` use the per-field overload on
[`crap.collections.<slug>.field_hook(field, fn)`](collections.md).

```lua
-- hooks/auto_slug.lua — applied to many collections' "slug" field
return crap.any.field_hook(function(value, context)
    if value and value ~= "" then return value end
    local title = context.data and context.data.title
    return title and crap.util.slugify(title) or value
end)
```

### `crap.any.access(fn)`

Wraps an access control function. `context: crap.AccessContext`. The
context type is uniform across collections — there's no per-collection
narrowing variant.

```lua
return crap.any.access(function(context)
    return context.user ~= nil and context.user.role == "admin"
end)
```

`context.user` is typed as `crap.AuthUser` (`crap.Document` plus an
`[string] any` index signature, since a project may have multiple
auth collections). When you know the auth collection, cast for strict
typing:

```lua
local user = context.user --[[@as crap.doc.Users]]
```

### `crap.any.auth_strategy(fn)`

Wraps a custom auth strategy's `authenticate` callback.
`context: crap.AuthStrategyContext` — `{ headers, collection }`.
Returns the user document or `nil`.

```lua
return crap.any.auth_strategy(function(context)
    local key = context.headers["x-api-key"]
    if not key then return nil end
    return crap.collections.find_by_id(context.collection, lookup_user(key))
end)
```

### `crap.any.job_handler(fn)`

Wraps a background job handler. `context: crap.JobHandlerContext`
(`{ data, job }`).

```lua
M.run = crap.any.job_handler(function(context)
    crap.log.info("Job attempt " .. context.job.attempt)
    -- context.data is the payload passed to crap.jobs.queue(...)
end)
```

### `crap.any.row_label(fn)`

Wraps a computed row label for array/blocks fields.

```lua
return crap.any.row_label(function(row)
    return row.heading or row.title or "(untitled)"
end)
```

### `crap.any.display_condition(fn)`

Wraps a display condition that doesn't need per-collection data
narrowing. For typed `data`, use
[`crap.collections.<slug>.condition(fn)`](collections.md).

```lua
return crap.any.display_condition(function(data)
    return { field = "kind", equals = "premium" }
end)
```

## When to reach for `crap.any` vs the per-slug accessor

| Situation | Use |
|---|---|
| Hook is specific to one collection's field | `crap.collections.<slug>.field_hook("<field>", fn)` |
| Hook applies to multiple fields of one collection | `crap.collections.<slug>.field_hook(fn)` |
| Hook applies to multiple collections | `crap.any.field_hook(fn)` |
| Collection hook on a specific collection (typed `ctx`) | `crap.collections.<slug>.hook(fn)` |
| Collection hook used across collections | `crap.any.collection_hook(fn)` |
| `before_delete` / `after_delete` / `before_broadcast` | `crap.any.collection_hook(fn)` (runtime always sends generic ctx) |
| Display condition tied to a collection's data | `crap.collections.<slug>.condition(fn)` |
| Access function (always uniform context) | `crap.any.access(fn)` or `crap.collections.<slug>.access(fn)` for discoverability |
| Auth strategy | `crap.any.auth_strategy(fn)` or `crap.collections.<slug>.auth_strategy(fn)` |
| Job handler | `crap.any.job_handler(fn)` |
