# Lifecycle Events

Seven lifecycle events fire during CRUD operations.

## Event Reference

| Event | Fires On | Mutable Data | CRUD Access | Notes |
|-------|----------|-------------|-------------|-------|
| `before_validate` | create, update | Yes | Yes | Normalize inputs before validation |
| `before_change` | create, update | Yes | Yes | Transform data after validation passes |
| `after_change` | create, update | No | No | Fire-and-forget. Notifications, cache invalidation. |
| `before_read` | find, find_by_id | No | No | Can abort the read by returning an error |
| `after_read` | find, find_by_id | Yes | No | Transform data before it reaches the client |
| `before_delete` | delete | No | Yes | Can abort the delete. CRUD access for cascading deletes. |
| `after_delete` | delete | No | No | Fire-and-forget. Cleanup tasks. |

## Write Lifecycle (create/update)

```
1. field before_validate hooks
2. collection before_validate hooks
3. global registered before_validate hooks
4. field validation (required, unique, custom validate)
5. field before_change hooks
6. collection before_change hooks
7. global registered before_change hooks
8. database write (INSERT or UPDATE)
9. join table write (has-many relationships, arrays)
10. transaction commit
11. field after_change hooks (background, no CRUD)
12. collection after_change hooks (background, no CRUD)
13. global registered after_change hooks (background, no CRUD)
```

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
4. collection after_delete hooks (background, no CRUD)
5. global registered after_delete hooks (background, no CRUD)
```
