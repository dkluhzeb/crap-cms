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
| `before_render` | No | Admin display enrichment — no transaction |
| `before_broadcast` | No | Live-event filtering/transform — no transaction |

This applies to all three hook levels (field, collection, registered).

## Available Functions

Inside hooks with CRUD access:

```lua
-- Collections
crap.collections.posts.find({ where = { status = "published" } })
crap.collections.posts.find_by_id("abc123")
crap.collections.audit_log.create({ action = "update", target = ctx.data.id })
crap.collections.posts.update(id, { view_count = views + 1 })
crap.collections.drafts.delete(old_id)

-- Globals
crap.globals.site_settings.get()
crap.globals.counters.update({ total_posts = count + 1 })
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

Calling `crap.collections.find()` etc. in a context with no database
connection (e.g. at the top level of a definition file) results in an error:

```
crap.collections CRUD functions need a database context — call
them inside a lifecycle hook (before_change, before_delete,
etc.), a job handler, a custom route handler, or wrap the call
in crap.transaction(fn)
```

Job handlers and custom route handlers run in **pool mode**: each CRUD
call batch pulls a fresh connection and opens its own transaction.
`crap.transaction(fn)` gives the same explicit transactional block
anywhere a pool context is available.

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
    local result = crap.collections.posts.find()
    if result.pagination.total_docs == 0 then
        crap.collections.posts.create({
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

Collection, global, and field-level access control functions run with CRUD access **on the calling operation's connection**. For write operations that means they run inside the operation's transaction (an access function's own writes roll back with the operation); for reads there is no transaction — each statement auto-commits. Access checks do **not** get a dedicated transaction of their own, so keep access functions read-only. (Which keys each surface honors — e.g. collections expose `read`/`draft`/`trash`/`versions`/`create`/`update`/`delete` while globals expose only `read`/`draft`/`update`/`versions` — is covered in [Access Control](../access-control/overview.md); the transaction behavior here applies to all of them.)

## Auth Strategies

Custom auth strategy `authenticate` functions run with CRUD access **inside a transaction that commits only when the strategy authenticates someone**. A strategy that returns `nil` or raises rolls its writes back — failed attempts are unauthenticated, attacker-controlled input, so persisting their side effects would let anyone grow the database from the login endpoint. "Find or create user" flows work unchanged (the create commits with the successful login). For failed-attempt bookkeeping use the built-in rate limiters (counters) and `crap.log` (observability) — both live outside this transaction.
