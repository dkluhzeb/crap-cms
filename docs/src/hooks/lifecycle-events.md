# Lifecycle Events

Nine lifecycle events fire during CRUD operations and admin page rendering.

## Event Reference

| Event | Fires On | Mutable Data | CRUD Access | Notes |
|-------|----------|-------------|-------------|-------|
| `before_validate` | create, update, update_many | Yes | Yes | Normalize inputs before validation |
| `before_change` | create, update, update_many | Yes | Yes | Transform data after validation passes |
| `after_change` | create, update, update_many | Yes | Yes | Runs inside the transaction. Audit logs, counters, side-effects. Errors roll back the entire operation. |
| `before_read` | find, find_by_id | No | No* | Can abort the read by returning an error |
| `after_read` | find, find_by_id | Yes | No | Transform data before it reaches the client |
| `before_delete` | delete, delete_many | No | Yes | Can abort the delete. CRUD access for cascading deletes. |
| `after_delete` | delete, delete_many | No | Yes | Runs inside the transaction. Cleanup, cascading deletes. Errors roll back the entire operation. |
| `before_broadcast` | create, update, delete | Yes (data) | No | Can suppress or transform live update events. See [Live Updates](../live-updates/hooks.md). |
| `before_render` | admin page render | Yes (context) | No | Runs before rendering admin pages. Receives the full template context and can modify it. Global-only (no collection-level refs). Useful for injecting global template data. |

*\* `before_read` hooks have no CRUD access when called from the gRPC API or admin UI. However, when triggered from a Lua CRUD call inside a hook (e.g., `crap.collections.find()` inside `before_change`), `before_read` hooks inherit the parent's transaction context and DO have CRUD access.*

## Document ID in Hook Context

In `after_change` and `after_delete` hooks, `context.data.id` contains the document ID. This is useful for queuing jobs or looking up the document after it's been written. In `before_delete` hooks, `context.data.id` is also available.

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
