# API Surface Comparison

This document compares the three API surfaces — **Admin UI**, **gRPC API**, and **Lua CRUD** (hook local API) — to track feature consistency.

## CREATE Lifecycle

| Step | Admin | gRPC | Lua CRUD |
|------|-------|------|----------|
| Access control (collection-level) | Yes | Yes | Yes (overrideAccess) |
| Field-level write stripping | Yes | Yes | Yes (overrideAccess) |
| Password extraction (auth) | Yes | Yes | Yes |
| before_validate (field + collection + registered) | Yes | Yes | Yes |
| Validation | Yes | Yes | Yes |
| before_change (field + collection + registered) | Yes | Yes | Yes |
| DB insert | Yes | Yes | Yes |
| Join table data (arrays, blocks, has-many) | Yes | Yes | Yes |
| Password hash + store | Yes | Yes | Yes |
| Versioning (status + snapshot + prune) | Yes | Yes | Yes |
| after_change (field + collection + registered) | Yes | Yes | Yes |
| Publish event (SSE/WebSocket) | Yes | Yes | No (in-transaction) |
| Verification email (auth + verify_email) | Yes | Yes | No |

## UPDATE Lifecycle

| Step | Admin | gRPC | Lua CRUD |
|------|-------|------|----------|
| Access control (collection-level) | Yes | Yes | Yes (overrideAccess) |
| Field-level write stripping | Yes | Yes | Yes (overrideAccess) |
| Password extraction (auth) | Yes | Yes | Yes |
| Unpublish path | Yes | Yes | Yes |
| before_validate (field + collection + registered) | Yes | Yes | Yes |
| Validation | Yes | Yes | Yes |
| before_change (field + collection + registered) | Yes | Yes | Yes |
| DB update (or draft-only version save) | Yes | Yes | Yes |
| Join table data | Yes | Yes | Yes |
| Password hash + store (normal path) | Yes | Yes | Yes |
| Versioning (status + snapshot + prune) | Yes | Yes | Yes |
| after_change (field + collection + registered) | Yes | Yes | Yes |
| Publish event | Yes | Yes | No (in-transaction) |

## DELETE Lifecycle

| Step | Admin | gRPC | Lua CRUD |
|------|-------|------|----------|
| Access control | Yes | Yes | Yes (overrideAccess) |
| before_delete (collection + registered) | Yes | Yes | Yes |
| DB delete | Yes | Yes | Yes |
| after_delete (collection + registered) | Yes | Yes | Yes |
| Upload file cleanup | Yes | Yes | No |
| Publish event | Yes | Yes | No (in-transaction) |

## FIND Lifecycle

| Step | Admin | gRPC | Lua CRUD |
|------|-------|------|----------|
| Access control (collection-level) | Yes | Yes | Yes (overrideAccess) |
| Constraint filter merging | Yes | Yes | Yes |
| Draft-aware filtering | Yes | Yes | Yes |
| before_read hooks | Yes | Yes | Yes |
| DB query + count | Yes | Yes | Yes |
| Hydrate join tables | Yes | Yes | Yes |
| Upload sizes assembly | Yes | Yes | Yes |
| after_read hooks (field + collection + registered) | Yes | Yes | Yes |
| Relationship population (depth) | Yes | Yes | Yes |
| Select field stripping | Yes | Yes | Yes |
| Field-level read stripping | Yes | Yes | Yes (overrideAccess) |

## FIND_BY_ID Lifecycle

| Step | Admin | gRPC | Lua CRUD |
|------|-------|------|----------|
| Access control (collection-level) | Yes | Yes | Yes (overrideAccess) |
| before_read hooks | Yes | Yes | Yes |
| Draft version overlay | Yes | Yes | Yes |
| Hydrate join tables | Yes | Yes | Yes |
| Upload sizes assembly | Yes | Yes | Yes |
| after_read hooks (field + collection + registered) | Yes | Yes | Yes |
| Relationship population (depth) | Yes | Yes | Yes |
| Select field stripping | Yes | Yes | Yes |
| Field-level read stripping | Yes | Yes | Yes (overrideAccess) |

## Remaining By-Design Differences

| Feature | Admin | gRPC | Lua CRUD | Reason |
|---------|-------|------|----------|--------|
| Event publishing | Yes | Yes | No | Lua runs inside the caller's transaction; event publishing is fire-and-forget after commit. The caller (admin/gRPC) publishes the event. |
| Upload file cleanup on delete | Yes | Yes | No | File I/O after commit is caller responsibility. |
| Verification email on create | Yes | Yes | No | Email sending is async, post-commit. |
| Locale from request | Yes | Yes | Explicit opt | Admin/gRPC infer from request; Lua passes explicitly via opts.locale. |
| Default depth | Varies | Config | 0 | Lua defaults to 0 to avoid N+1 in hooks. Callers pass depth explicitly. |
