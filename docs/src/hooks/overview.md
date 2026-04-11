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

After-write hooks (`after_change`, `after_delete`) also have CRUD access and run inside the same transaction. Errors roll back the entire operation.

After-read hooks (`after_read`) do NOT have CRUD access.

See [Transaction Access](transaction-access.md) for details.

## Concurrency

Hooks execute in a pool of Lua VMs, allowing concurrent hook execution across requests. The pool size is configurable:

```toml
[hooks]
vm_pool_size = 8  # default: number of CPU cores
```

Each VM is fully initialized at startup with the same configuration (package paths, API registration, CRUD functions, `init.lua` execution). When a request needs to execute a hook, it acquires a VM from the pool and returns it when done. This prevents hook execution from serializing under concurrent load.

## Resource Limits

Lua VMs have configurable instruction, memory, and recursion limits to prevent runaway hooks:

```toml
[hooks]
max_depth = 3                      # max hook recursion depth (hook → CRUD → hook; 0 = no hooks from Lua CRUD)
max_instructions = 10000000        # per hook invocation (0 = unlimited)
max_memory = 52428800              # per VM in bytes, 50 MB (0 = unlimited)
allow_private_networks = false     # block HTTP to internal IPs
http_max_response_bytes = 10485760 # 10 MB (increase for large file downloads)
```

- **Instruction limit** — a hook that exceeds the instruction count is terminated with an error. The default (10M) is generous for complex hooks.
- **Memory limit** — caps total Lua memory per VM. Exceeding it raises a memory error.
- **Private network blocking** — `crap.http.request` resolves hostnames and rejects private/loopback/link-local IPs unless `allow_private_networks = true`.
- **`crap.crypto.random_bytes`** — capped at 1 MB per call.

## State & Module Caching

Lua's `require` function caches modules in `package.loaded`. This means module-level variables persist across requests on the same VM:

```lua
-- hooks/posts.lua
local M = {}
local counter = 0  -- persists across requests!

function M.before_change(ctx)
    counter = counter + 1  -- increments forever on this VM
    return ctx
end

return M
```

To avoid cross-request state leaks, keep hook functions stateless — use the `ctx` table for input/output, and `crap.collections.*` for persistent storage. If you need request-scoped state, store it in `ctx.context` (the request-scoped shared table — see [Hook Context](hook-context.md#context-request-scoped-shared-table)), not module-level locals.

Module-level constants and utility functions are fine — only mutable state is the concern.

> **Important: VM pool behavior.** Since HookRunner uses a pool of Lua VMs, global state in Lua modules persists across requests **within the same VM** but is **not shared across VMs**. Each VM in the pool has its own independent copy of module-level variables. This means:
>
> - Module-level variables can act as in-memory caches, but different requests may hit different VMs and see different cached values.
> - Cached state is **not** consistent across the pool — one VM's counter may be at 5 while another is at 12.
> - All cached state is **lost on server restart** (VMs are re-initialized from scratch).
>
> If you need shared, consistent state, use `crap.collections.*` or `crap.globals.*` to persist to the database.
