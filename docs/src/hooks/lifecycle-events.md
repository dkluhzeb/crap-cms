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
| `before_render` | admin page render | Yes (context) | Read-only† | Runs before rendering admin pages. Receives the full template context plus a page-identity table, and can modify the context. Global-only (no collection-level refs). |

*† `before_render` gets **read-only** CRUD on authenticated admin pages — see [`before_render`](#before_render) below. Unauthenticated pages and error pages get none.*

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

Fires before an admin page template is rendered, from every admin page handler —
dashboard, collection list/edit, delete confirm, version list/restore, globals,
custom pages, login, forgot/reset password, and the error pages.

**Global hooks only** (no collection-level refs). Errors, non-table returns, and
conversion failures log a warning and fall back to the unmodified context.

### Arguments

```lua
crap.hooks.register("before_render", function(ctx, info)
    ...
end)
```

| Argument | What it is |
|----------|------------|
| `ctx` | The full template context — the JSON object handed to the template engine, as a Lua table. |
| `info` | Which page is rendering. See below. |

`info` carries:

| Field | Type | Description |
|-------|------|-------------|
| `info.page` | string | The page discriminant, same value as `ctx.page.type` (`"dashboard"`, `"collection_items"`, `"error_404"`, …). |
| `info.template` | string | The template being rendered, e.g. `"collections/items"`. Reflects the built-in name even when an overlay template has replaced it. |
| `info.collection` | string? | Collection slug, on pages scoped to one collection. |
| `info.global` | string? | Global slug, on pages scoped to one global. |

Use `info` to scope a hook to the pages it applies to — one line, instead of
guessing from which context keys happen to exist:

```lua
crap.hooks.register("before_render", function(ctx, info)
    if info.page ~= "dashboard" then return end
    ctx.banner_message = "Maintenance window 02:00 UTC tonight"
    return ctx
end)
```

### Return values

- A table — becomes the context for the remaining hooks and the renderer.
- `nil` — keeps the current context. Lua tables are references, so mutating
  `ctx` in place is enough; returning it is conventional and harmless.
- Anything else — logged as a warning, ignored.

Every registered hook shares **one** Lua table, converted from and back to JSON
exactly once per render however many hooks are registered. A hook therefore sees
what earlier hooks wrote.

### Database access

On an **authenticated admin page** the hook gets **read-only** database access, so
it can build page data from real content:

```lua
crap.hooks.register("before_render", function(ctx, info)
    if info.page ~= "dashboard" then return end

    ctx.pending_orders = crap.collections.orders.count({
        where = { status = { equals = "pending" } },
    })
    return ctx
end)
```

Reads run **as the signed-in admin**, with access control applied exactly as
anywhere else — so a hook cannot surface rows the viewer is not allowed to see
unless it explicitly passes `override_access = true`. With
`[admin] require_auth = false` there is no signed-in user, so reads run with no
identity and your access rules see `ctx.user == nil`.

Writes are **refused**, with an error naming the alternative. Two reasons:

1. A page render is a `GET`. Write-capable pool access takes the single SQLite
   writer even for a `count`, which would serialize every admin page load
   against every real write.
2. A render hook that could write would turn viewing a page into a mutation,
   with no request an operator could point at as the cause.

Do writes from a lifecycle hook, a [job handler](../jobs/overview.md), or a
[custom route](../lua-api/routes.md) instead. `crap.transaction(fn)` is refused
here for the same reason.

The same access applies to [`crap.template_data`](../admin-ui/scenarios/04-dashboard-widget.md)
functions on that page. They are the other render-time extension point —
`{{data "name"}}` in a template — and giving one a database but not the other
would send anyone reaching for the purpose-built helper down a dead end.

**Unauthenticated pages** (login, forgot/reset password, MFA) and **error pages**
(400/403/404/500) run the hook with **no** database access at all. That is a
boundary rather than an optimization: there is no signed-in user to scope a read
by, so anything a hook read would either be denied or have to bypass access
control — and its output would land on a page served to an anonymous visitor.
Error pages additionally have to render when the database is the thing that
failed.
