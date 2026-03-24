# Transaction Access

Hooks can call back into the Crap CMS CRUD API. Whether a hook has CRUD access depends on the lifecycle event.

## Which Hooks Get CRUD Access?

| Event | CRUD Access | Reason |
|-------|-------------|--------|
| `before_validate` | Yes | Runs inside the write transaction |
| `before_change` | Yes | Runs inside the write transaction |
| `after_change` | **Yes** | Runs inside the write transaction, after the DB operation |
| `before_read` | No | Read operations don't open a write transaction |
| `after_read` | **No** | Fire-and-forget, no transaction |
| `before_delete` | Yes | Runs inside the delete transaction |
| `after_delete` | **Yes** | Runs inside the delete transaction, after the DB delete |

This applies to all three hook levels (field, collection, registered).

## Available Functions

Inside hooks with CRUD access:

```lua
-- Collections
crap.collections.find("posts", { where = { status = "published" } })
crap.collections.find_by_id("posts", "abc123")
crap.collections.create("audit_log", { action = "update", target = ctx.data.id })
crap.collections.update("posts", id, { view_count = views + 1 })
crap.collections.delete("drafts", old_id)

-- Globals
crap.globals.get("site_settings")
crap.globals.update("counters", { total_posts = count + 1 })
```

## Transaction Sharing

CRUD calls inside hooks share the **same database transaction** as the parent operation. This means:

- If the hook creates a document and the parent operation later fails, the created document is rolled back
- If the hook fails, the entire parent operation rolls back
- All changes are atomic — either everything commits or nothing does

This applies to **all write hooks**: `before_validate`, `before_change`, `after_change`, `before_delete`, and `after_delete`.

## Error Handling

If any hook (before or after) returns an error or throws a Lua error, the entire transaction is rolled back and the operation fails with an error message. This includes after-hooks — an `after_change` error will roll back the main DB operation too.

## Calling CRUD Outside Hooks

Calling `crap.collections.find()` etc. outside a hook context (no active transaction) results in an error:

```
crap.collections CRUD functions are only available inside hooks
with transaction context (before_change, before_delete, etc.)
```

## on_init Hooks

The `[hooks] on_init` list in `crap.toml` runs at startup with CRUD access. All `on_init` hooks share a single database transaction — if any hook fails, all changes are rolled back. This makes seeding and startup migrations atomic:

```toml
[hooks]
on_init = ["hooks.seed.run"]
```

```lua
-- hooks/seed.lua
local M = {}

function M.run(ctx)
    local result = crap.collections.find("posts")
    if result.pagination.totalDocs == 0 then
        crap.collections.create("posts", {
            title = "Welcome",
            slug = "welcome",
            status = "published",
            content = "Welcome to your new site!",
        })
        crap.log.info("Seeded initial post")
    end
    return ctx
end

return M
```

If an `on_init` hook fails, the server aborts startup.

## Access Control Functions

Access control functions (`access.read`, `access.create`, `access.update`, `access.delete` on collections/globals and `access.read`, `access.create`, `access.update` on fields) run with CRUD access inside their own transaction. Each access check gets a dedicated transaction that commits on success or rolls back on error.

## Auth Strategies

Custom auth strategy `authenticate` functions run with CRUD access inside a transaction. All strategies for a given request share a single transaction — if a strategy authenticates successfully, the transaction commits. If all strategies fail, the transaction rolls back.
