# API Surface Comparison

This document compares the API surfaces — **Admin UI**, **gRPC API**, and **Lua CRUD** (hook local API) — to track feature consistency.

**MCP** is the fourth surface; its tools call the same service-layer
operations as gRPC (including event publishing, with an opt-out
`events` argument), so the **gRPC column applies to MCP** throughout
this document.

## CREATE Lifecycle

| Step | Admin | gRPC | Lua CRUD |
|------|-------|------|----------|
| Access control (collection-level) | Yes | Yes | Yes (override_access) |
| Field-level write stripping | Yes | Yes | Yes (override_access) |
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
| Verification email (auth + verify_email) | Yes | Yes | Queued (flushed by the caller after commit) |

## UPDATE Lifecycle

| Step | Admin | gRPC | Lua CRUD |
|------|-------|------|----------|
| Access control (collection-level) | Yes | Yes | Yes (override_access) |
| Field-level write stripping | Yes | Yes | Yes (override_access) |
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
| Access control | Yes | Yes | Yes (override_access) |
| before_delete (collection + registered) | Yes | Yes | Yes |
| DB delete | Yes | Yes | Yes |
| after_delete (collection + registered) | Yes | Yes | Yes |
| Upload file cleanup | Yes | Yes | Yes |
| Publish event | Yes | Yes | No (in-transaction) |

## FIND Lifecycle

| Step | Admin | gRPC | Lua CRUD |
|------|-------|------|----------|
| Access control (collection-level) | Yes | Yes | Yes (override_access) |
| Constraint filter merging | Yes | Yes | Yes |
| Draft-aware filtering | Yes | Yes | Yes |
| before_read hooks | Yes | Yes | Yes |
| DB query + count | Yes | Yes | Yes |
| Hydrate join tables | Yes | Yes | Yes |
| Upload sizes assembly | Yes | Yes | Yes |
| after_read hooks (field + collection + registered) | Yes | Yes | Yes |
| Relationship population (depth) | Yes | Yes | Yes |
| Select field stripping | Yes | Yes | Yes |
| Field-level read stripping | Yes | Yes | Yes (override_access) |

## FIND_BY_ID Lifecycle

| Step | Admin | gRPC | Lua CRUD |
|------|-------|------|----------|
| Access control (collection-level) | Yes | Yes | Yes (override_access) |
| before_read hooks | Yes | Yes | Yes |
| Draft version overlay | Yes | Yes | Yes |
| Hydrate join tables | Yes | Yes | Yes |
| Upload sizes assembly | Yes | Yes | Yes |
| after_read hooks (field + collection + registered) | Yes | Yes | Yes |
| Relationship population (depth) | Yes | Yes | Yes |
| Select field stripping | Yes | Yes | Yes |
| Field-level read stripping | Yes | Yes | Yes (override_access) |

## Operation Availability

The lifecycles above cover the shared single-document operations. The
full operation set per surface:

- **All surfaces** (admin, gRPC, Lua, MCP): create, update, delete,
  undelete, unpublish, find, find-by-id, count, validate, version
  list/restore. The admin UI exposes some of these through UI flows
  rather than standalone calls — validate via the inline-validation
  endpoints, count via list pagination, versions via the history pages.
- **API surfaces only** (gRPC, Lua, MCP): the bulk operations
  `create_many` / `update_many` / `delete_many`. The admin UI operates
  on single documents by design (except empty-trash, which is a bulk
  hard-delete of trashed items).

## Remaining By-Design Differences

| Feature | Admin | gRPC | Lua CRUD | Reason |
|---------|-------|------|----------|--------|
| Event publishing | Yes | Yes | No | Lua runs inside the caller's transaction; event publishing is fire-and-forget after commit. The caller (admin/gRPC) publishes the event. |
| Upload file cleanup on delete | Yes | Yes | Yes | Lua CRUD reads ConfigDir from Lua app_data; admin/gRPC clean up after commit. |
| Verification email on create | Yes | Yes | Queued | Email sending is async, post-commit. Lua runs inside the caller's transaction, so it pushes the email onto a verification queue that the caller flushes after commit. |
| Invalid filter / sort input | 400 page | INVALID_ARGUMENT | Lua error | All surfaces hard-error on unknown operators, unknown fields, and malformed clauses — nothing is silently dropped. |
| Locale from request | Yes | Yes | Explicit opt | Admin/gRPC infer from request; Lua passes explicitly via opts.locale. |
| Default depth | Varies | Config | 0 | Lua defaults to 0 to avoid N+1 in hooks. Callers pass depth explicitly. |
