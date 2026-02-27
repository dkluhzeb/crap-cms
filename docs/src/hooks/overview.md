# Hooks

Hooks let you intercept and modify data at every stage of a document's lifecycle. They're the primary extension mechanism in Crap CMS.

## Three Levels of Hooks

1. **Field-level hooks** — per-field value transformers. Defined on individual `FieldDefinition` entries.
2. **Collection-level hooks** — per-collection lifecycle hooks. Defined on `CollectionDefinition` or `GlobalDefinition`.
3. **Globally registered hooks** — fire for all collections. Registered via `crap.hooks.register()` in `init.lua`.

All hooks at all levels run in this order for each lifecycle event:

```
field-level → collection-level → globally registered
```

## Hook References

Collection-level and field-level hooks are string references in `module.function` format:

```lua
hooks = {
    before_change = { "hooks.posts.auto_slug" },
}
```

This resolves to `require("hooks.posts").auto_slug` via Lua's module system. The config directory is on the package path, so `hooks/posts.lua` should return a module table:

```lua
-- hooks/posts.lua
local M = {}

function M.auto_slug(ctx)
    if ctx.data.slug == nil or ctx.data.slug == "" then
        ctx.data.slug = crap.util.slugify(ctx.data.title or "")
    end
    return ctx
end

return M
```

## No Closures

Hook references are always strings, never Lua functions. This keeps collection definitions serializable (important for the future visual builder).

The one exception is `crap.hooks.register()`, which takes a function directly — but it's called in `init.lua`, not in collection definitions.

## CRUD Access in Hooks

Before-event hooks (`before_validate`, `before_change`, `before_delete`) have full CRUD access via the `crap.collections.*` and `crap.globals.*` APIs. They share the parent operation's database transaction.

After-event hooks (`after_change`, `after_read`, `after_delete`) do NOT have CRUD access. They fire in the background after the transaction commits.

See [Transaction Access](transaction-access.md) for details.

## Concurrency

Hooks execute in a pool of Lua VMs, allowing concurrent hook execution across requests. The pool size is configurable:

```toml
[hooks]
vm_pool_size = 4  # default: min(available_parallelism, 8)
```

Each VM is fully initialized at startup with the same configuration (package paths, API registration, CRUD functions, `init.lua` execution). When a request needs to execute a hook, it acquires a VM from the pool and returns it when done. This prevents hook execution from serializing under concurrent load.
