# Hook Context

Collection-level hooks receive a context table and must return a (potentially modified) context table.

## Context Shape

```lua
{
    collection = "posts",       -- Collection slug
    operation = "create",       -- "create", "update", "delete", "find", "find_by_id", "get", or "init"
    document_id = "abc123",     -- Affected document id; nil only in create's before-hooks (no row yet)
    data = {                    -- Document data (mutable in before-write hooks)
        title = "Hello World",
        slug = "hello-world",
        status = "draft",
        id = "abc123",          -- Present on update/delete, absent on create
        created_at = "...",     -- Present on read/update
        updated_at = "...",
    },
    locale = "en",              -- Content locale this op targets (nil only when localization is disabled)
    draft = true,               -- Whether this is a draft save (versioned collections only)
    hook_depth = 0,             -- Current recursion depth (0 = top-level, 1+ = from Lua CRUD in hooks)
    context = {                 -- Per-operation shared table (one write lifecycle — see "Context" below)
        -- Hooks can read and write arbitrary keys here to share data
    },
    user = {                    -- Authenticated user document (nil if unauthenticated)
        id = "user_123",
        email = "admin@example.com",
        role = "admin",
        -- ... all fields from the auth collection
    },
    ui_locale = "en",           -- Admin UI locale (nil if not set)
    options = {                 -- Per-config options (nil for a bare ref — see below)
        from = "title",
        to = "slug",
    },
}
```

## Per-Config Options (`ctx.options`)

A hook ref is normally a bare string. To reuse **one** hook function across
collections (or fields) with different configuration, declare the ref as a
`{ ref, options }` table — the `options` table is handed to the hook as
`ctx.options` (it is `nil` for a bare-string ref):

```lua
crap.collections.define("posts", {
    fields = { ... },
    hooks = {
        before_change = {
            "hooks.posts.touch",                       -- bare ref: ctx.options is nil
            {
                ref = "hooks.shared.slugify",
                options = { from = "title", to = "slug" },
            },
        },
    },
})
```

```lua
-- hooks/shared/slugify.lua — one reusable hook, parameterized per collection.
return function(ctx)
    local o = ctx.options
    if o and (not ctx.data[o.to] or ctx.data[o.to] == "") then
        ctx.data[o.to] = crap.util.slugify(ctx.data[o.from] or "")
    end
    return ctx
end
```

`ctx.options` is the same per-config table at **every** site a ref can be
declared: collection/global lifecycle hooks, field hooks, `access` rules, field
`validate` / `required_when`, `admin.condition`, `auth` `strategy`
`authenticate`, `crap.jobs.define` `handler` / `access`, `crap.pages.register`
`access`, the collection `live.filter` broadcast gate, and the `[admin] access`
panel gate in `crap.toml` (a TOML inline table: `access = { ref = "...",
options = { ... } }`). The bare-string form stays valid everywhere, so this is
purely additive.

## Typed Contexts

The type generator (`crap-cms typegen`) emits per-collection context types with typed
`data` fields for IDE autocomplete:

- **Collections:** `crap.hook.{PascalCase}` — e.g., `crap.hook.Posts` has
  `data: crap.data.Posts` and `collection: "posts"` (literal)
- **Globals:** `crap.hook.global_{slug}` — e.g., `crap.hook.global_site_settings`
  has `data: crap.global_data.SiteSettings`

Use the typed context for hooks that target a specific collection:

```lua
---@param context crap.hook.Posts
---@return crap.hook.Posts
return function(context)
    -- context.data.title, context.data.slug, etc. autocomplete
    return context
end
```

**Delete hooks** (`before_delete` / `after_delete`) receive the deleted
document's full field data in `data`, alongside `id` (and `soft_delete = true`
for a soft delete). The snapshot is captured before the row is removed, so
`after_delete` can still see the content even on a hard delete (where the row no
longer exists to re-fetch). They use the generic `crap.HookContext`:

```lua
---@param context crap.HookContext
---@return crap.HookContext
return function(context)
    local id = context.data.id
    local title = context.data.title  -- the deleted document's fields
    return context
end
```

> A collection field literally named `id` or `soft_delete` is shadowed by those
> context keys in delete hooks.

For shared hooks that fire across multiple collections (e.g., via
`crap.hooks.register()`), use the generic `crap.HookContext`.

## Data Mutation

In **before-write hooks** (`before_validate`, `before_change`), you can modify `ctx.data` and return the modified context. The changes flow through to the database write.

```lua
function M.auto_slug(ctx)
    if not ctx.data.slug or ctx.data.slug == "" then
        ctx.data.slug = crap.util.slugify(ctx.data.title or "")
    end
    return ctx
end
```

In **after-read hooks**, you can also modify `ctx.data` to transform the response before it reaches the client.

## Return Value

Hooks must return the context table (or a new table with `data`). If a hook returns:

- A table with a `data` key — the data is replaced
- A table without a `data` key — the original data is kept
- A non-table value — the original context is kept

## System Fields in Data

| Field | Present When | Description |
|-------|-------------|-------------|
| `id` | update, delete, read | Document ID |
| `created_at` | read, update | ISO 8601 timestamp |
| `updated_at` | read, update | ISO 8601 timestamp |
| `user` | write hooks, after_read (nil if unauthenticated) | Authenticated user document |
| `ui_locale` | write hooks, after_read (nil if not set) | Admin UI locale code |

On `create`, `id` is not yet assigned (it's generated by the database write).

## Draft Field

For versioned collections with `drafts = true`, the context includes a `draft` field:

| Value | Meaning |
|-------|---------|
| `true` | This is a draft save (required field validation is skipped) |
| `false` | This is a publish save (full validation applied) |
| `nil` | Collection does not have versioning enabled |

You can use this in hooks to customize behavior based on publish state:

```lua
function M.before_change(ctx)
    if ctx.draft then
        -- Draft save: skip expensive operations
        return ctx
    end
    -- Publishing: run full processing
    ctx.data.published_at = os.date("!%Y-%m-%d %H:%M:%S")
    return ctx
end
```

## User

The `user` field contains the full authenticated user document from the auth collection, or `nil` if the request is unauthenticated (or no auth collection exists). This is the same user document used by access control functions.

```lua
function M.before_change(ctx)
    if ctx.user then
        ctx.data.last_edited_by = ctx.user.email
    end
    return ctx
end
```

## UI Locale

The `ui_locale` field contains the admin UI locale code (e.g., `"en"`, `"de"`), or `nil` if not set. This is useful for returning user-facing messages (e.g., validation errors) in the correct language.

```lua
function M.validate_title(value, ctx)
    if not value or value == "" then
        if ctx.ui_locale == "de" then
            return "Titel ist erforderlich"
        end
        return "Title is required"
    end
    return true
end
```

## Hook Depth

The `hook_depth` field tracks how deep in the hook→CRUD→hook chain the current execution is:

| Value | Meaning |
|-------|---------|
| `0` | Top-level call from gRPC API or admin UI |
| `1` | Called from Lua CRUD inside a hook |
| `2+` | Deeper recursion (hook called CRUD which triggered another hook) |

When `hook_depth` reaches `hooks.max_depth` (default: 3, configurable in `crap.toml`),
hooks are automatically skipped but the DB operation still executes. This prevents infinite
recursion when hooks create/update documents in the same collection.

```lua
function M.audit_hook(ctx)
    -- Only audit at the top level, not from recursive hook calls
    if ctx.hook_depth >= 1 then
        return ctx
    end
    crap.collections.audit_log.create({
        action = ctx.operation,
        collection = ctx.collection,
    })
    return ctx
end
```

## Reading the Previous Document

In a **before-write hook** (`before_validate`, `before_change`) the document has
not been written yet, so the *currently persisted* row is still the old state.
Fetch it on demand with `crap.collections.find_by_id` using `ctx.document_id`
(the affected document's id — present on update/delete, `nil` on create):

```lua
-- Reject any price decrease by comparing against the persisted value.
function M.price_increase_only(ctx)
    if ctx.operation ~= "update" then
        return ctx
    end
    local old = crap.collections.find_by_id(ctx.collection, ctx.document_id, { overrideAccess = true })
    if old and ctx.data.price < old.price then
        error("price may only increase")
    end
    return ctx
end
```

This costs one read **only when a hook asks for it** — hooks that don't need the
old document pay nothing. In an `after_change` hook the row already holds the
*new* state, so to compare old-vs-new after the write, capture the old value in
`before_change` and read it back via `ctx.context` (next section).

## Context (Per-Operation Shared Table)

The `context` field is a table scoped to a **single write operation** — one
`create` / `update` / `delete` lifecycle and its `before_*` → `after_*` hooks.
It lets those hooks share data without relying on module-level state. It starts
empty at the beginning of each operation and is the canonical way to carry a
value from a before-hook into an after-hook.

> **Scope:** it is *not* request-scoped. A bulk operation that writes 100
> documents runs 100 separate `before_*`→`after_*` cycles, each with its own
> fresh `context`; nested CRUD (a hook that calls `crap.collections.*`) gets its
> own `context` too. Data does not leak between documents or between operations.

Use it to carry the pre-write document into `after_change` — the one thing the
on-demand pattern above can't do post-write, since the row then holds the new
state:

```lua
-- before_change: capture the persisted old value
function M.capture_status(ctx)
    local old = crap.collections.find_by_id(ctx.collection, ctx.document_id, { overrideAccess = true })
    ctx.context.old_status = old and old.status
    return ctx
end

-- after_change: react to the committed change using the captured value
function M.audit_status(ctx)
    if ctx.context.old_status ~= ctx.data.status then
        crap.collections.audit_log.create({
            collection = ctx.collection,
            from = ctx.context.old_status,
            to = ctx.data.status,
        })
    end
    return ctx
end
```

> **Never use Lua globals (module-level variables) to pass data between hooks.**
> Hooks run on a **pool of Lua VMs**, and `before_*` and `after_*` may execute on
> *different* VMs — so a global set in one hook may be invisible in the next.
> Worse, pooled VMs are reused across requests, so a global written by one
> request can leak into another user's request on the same VM. `ctx.context` is
> immune to both: it is threaded between hooks at the Rust level, independent of
> which VM runs each hook. (See [Hooks Overview](overview.md#state--module-caching)
> for the VM-pool model.)
