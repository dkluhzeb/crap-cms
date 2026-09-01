# Lifecycle Events

Nine lifecycle events fire during CRUD operations and admin page rendering.

## Event Reference

| Event | Fires On | Mutable Data | CRUD Access | Notes |
|-------|----------|-------------|-------------|-------|
| `before_validate` | create, update, update_many | Yes | Yes | Normalize inputs before validation |
| `before_change` | create, update, update_many | Yes | Yes | Transform data after validation passes |
| `after_change` | create, update, update_many | Yes | Yes | Runs inside the transaction. Audit logs, counters, side-effects. Errors roll back the entire operation. |
| `before_read` | find, find_by_id | No | No* | Can abort the read by returning an error. Can seed `ctx.context` for `after_read` (shared per-read). |
| `after_read` | find, find_by_id | Yes | No | Transform data before it reaches the client |
| `before_delete` | delete, delete_many | No | Yes | Can abort the delete. CRUD access for cascading deletes. |
| `after_delete` | delete, delete_many | No | Yes | Runs inside the transaction. Cleanup, cascading deletes. Errors roll back the entire operation. |
| `before_broadcast` | create, update, delete | Yes (data) | No | Can suppress or transform live update events. See [Live Updates](../live-updates/hooks.md). |
| `before_render` | admin page render | Yes (context) | No | Runs before rendering admin pages. Receives the full template context and can modify it. Global-only (no collection-level refs). Useful for injecting global template data. |

*\* `before_read` hooks have no CRUD access when called from the gRPC API or admin UI. However, when triggered from a Lua CRUD call inside a hook (e.g., `crap.collections.find()` inside `before_change`), `before_read` hooks inherit the parent's transaction context and DO have CRUD access.*

## Document ID in Hook Context

Every **write** hook context (and `after_read`, which runs per document) exposes the affected document's id as the top-level `ctx.id` (consistent with the field-hook, validator, and access contexts) — `nil` in create's before-hooks, where no row exists yet. `before_read` has no `ctx.id`: it fires once per read *operation* (a `find` matches many documents), before any specific document is known. In `after_change` and `after_delete` hooks the id is *also* present as `context.data.id`. This is useful for queuing jobs or looking up the document after it's been written. In `before_delete` hooks, `context.data.id` is likewise available.

`before_delete` and `after_delete` additionally receive the deleted document's full field data in `context.data` (captured before the row is removed), so you don't need to re-fetch — and on a hard delete you couldn't, since the row is already gone by `after_delete`.

## Write Lifecycle (create/update)

```
1. field before_validate hooks (CRUD access)
2. collection before_validate hooks (CRUD access)
3. global registered before_validate hooks (CRUD access)
4. field validation (required, unique, custom validate)
5. field before_change hooks (CRUD access)
6. collection before_change hooks (CRUD access)
7. global registered before_change hooks (CRUD access)
8. database write (INSERT or UPDATE)
9. join table write (has-many relationships, arrays)
10. field after_change hooks (CRUD access, same transaction)
11. collection after_change hooks (CRUD access, same transaction)
12. global registered after_change hooks (CRUD access, same transaction)
13. transaction commit
14. live setting check (background)
15. before_broadcast hooks (background, no CRUD)
16. EventBus publish (if not suppressed)
```

## Bulk Operations (update_many/delete_many)

`update_many` and `delete_many` run the same per-document lifecycle as their single-document counterparts. Each matched document goes through the full hook pipeline individually, all within a single transaction (all-or-nothing).

**update_many** runs steps 1–12 above for each document. Key differences from single-document `update`:
- Only provided fields are written (partial update). Absent fields — including checkboxes — are left unchanged.
- Password updates are rejected. Use single-document `Update` instead.
- Hook-modified data is captured and written (hooks can transform the data).
- Set `hooks = false` to skip all hooks and validation for performance.

**delete_many** runs the delete lifecycle (steps 1–5 below) for each document.

## Read Lifecycle (find/find_by_id)

```
1. collection before_read hooks
2. global registered before_read hooks
3. database query
4. field after_read hooks
5. collection after_read hooks
6. global registered after_read hooks
```

> **`after_read` errors fail *open*** — this is the one event that does **not**
> abort on error. It is the read-path enrichment/transform layer, not the access
> boundary (field-level read access is enforced separately and fails *closed*),
> so a buggy `after_read` hook must not be able to break every read, list page,
> and live event for a collection. On error the document is logged and returned
> **unmodified** (in its original, already-access-stripped form). Put redaction in
> field `access.read`, not in `after_read`; if you need strict transform
> behavior, handle errors inside your hook. Every other event aborts on error.

## Delete Lifecycle

```
1. collection before_delete hooks (CRUD access)
2. global registered before_delete hooks (CRUD access)
3. database delete
4. collection after_delete hooks (CRUD access, same transaction)
5. global registered after_delete hooks (CRUD access, same transaction)
6. transaction commit
7. live setting check (background)
8. before_broadcast hooks (background, no CRUD)
9. EventBus publish (if not suppressed)
```

## `before_broadcast`

Fires after a `create`, `update`, or `delete` has been committed and the live setting
check has passed, but **before** the event is dispatched on the EventBus to live
subscribers (SSE, gRPC `Subscribe`). Runs in a background `spawn_blocking` task — never
blocks the response to the originating request.

**No CRUD access** (the transaction is already closed).

The hook receives a context table with `collection`, `operation` (`"create"`,
`"update"`, or `"delete"`), and `data` (the document payload that would be broadcast).

**Return values:**

- A table — broadcast continues with the (possibly mutated) `data`.
- `nil` or `false` — the broadcast is **suppressed** for this subscriber wave; no
  event is dispatched.

```lua
crap.hooks.register("before_broadcast", function(ctx)
    -- Don't broadcast drafts
    if ctx.data.status == "draft" then
        return nil
    end

    -- Strip a sensitive field from the broadcast payload only
    ctx.data.internal_notes = nil
    return ctx
end)
```

Collection-level `before_broadcast` hook refs run before global registered hooks. A
suppression at any stage stops the rest of the chain.

## `before_render`

Fires before an admin page template is rendered. The hook receives the full template
context (the JSON object passed to the template engine) as a Lua table, and may return
a modified table to inject globals, sidebar items, or additional template variables.

**No CRUD access**, **global hooks only** (no collection-level refs). Errors, non-table
returns, and conversion failures log a warning and fall back to the unmodified context.

**Return values:**

- A table — the new template context.
- `nil` — the original context is preserved (no-op).
- Anything else — logged as a warning, ignored.

```lua
crap.hooks.register("before_render", function(ctx)
    -- Inject a banner shown in the admin layout
    ctx.banner_message = "Maintenance window 02:00 UTC tonight"
    return ctx
end)
```

Fires from every admin page handler (dashboard, list, edit, delete confirm, version
list/restore, login, forgot/reset password, etc.). Inspect `ctx` to branch on the page
being rendered.
