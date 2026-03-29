# Delete Protection

Every collection table has a `_ref_count` column that tracks how many documents reference it. When `_ref_count > 0`, the document cannot be deleted — this prevents orphaned references across collections.

## How It Works

When document A has a relationship field pointing to document B, B's `_ref_count` is incremented. When A is updated to point elsewhere or hard-deleted, B's `_ref_count` is decremented. This makes delete protection an **O(1)** check — no scanning required.

Reference counting covers all relationship types:

| Type | Storage | Tracked |
|------|---------|---------|
| Has-one relationship | Column on parent table | Yes |
| Has-many relationship | Junction table | Yes |
| Polymorphic (has-one/many) | `collection/id` format | Yes |
| Localized relationships | Per-locale columns | Yes |
| Upload fields | Same as relationship | Yes |
| Array sub-field refs | Column in array table | Yes |
| Block sub-field refs | JSON in blocks table | Yes |
| Global outgoing refs | Global table columns | Yes |

## Scope

Delete protection applies to **all collections**, not just uploads. Any document referenced by another document is protected.

## Soft Delete Interaction

Soft-deleting a document does **not** adjust ref counts. The outgoing references remain counted because:

- Soft-deleted documents can be restored, so their references should remain tracked
- Trashed documents still "own" their references in the database

Only **hard deletion** (permanent) decrements ref counts on the targets.

Soft-deleted documents that are referenced by other documents can still be trashed — the ref count check only blocks deletion of the *target* document.

## Admin UI

The delete confirmation page shows a warning when a document has `_ref_count > 0`:

> **This document is referenced by other content.**
> Referenced by 3 document(s).
> [Show details]

Clicking **Show details** lazy-loads the full list of referencing documents, fields, and counts via the back-references API endpoint.

## API Behavior

### Admin & gRPC

Attempting to delete a document with `_ref_count > 0` returns an error:

```
Cannot delete '<id>' from '<collection>': referenced by N document(s)
```

### Lua API

```lua
-- Single delete: fails with error if referenced
local ok, err = pcall(crap.collections.delete, "media", "m1")

-- Bulk delete: silently skips referenced documents
local result = crap.collections.delete_many("media", {
    where = { status = { equals = "unused" } }
})
-- result.deleted only includes documents that were actually deleted
```

### Force Hard Delete

The `forceHardDelete` option bypasses the ref count check. This is used internally for **Empty Trash** operations and can be used in Lua hooks:

```lua
crap.collections.delete("media", "m1", {
    forceHardDelete = true  -- skips ref count check
})
```

## Back-References API

To see which documents reference a target, use the back-references endpoint:

```
GET /admin/collections/{slug}/{id}/back-references
```

Returns a JSON array:

```json
[
    {
        "owner_slug": "posts",
        "owner_label": "Posts",
        "field_name": "image",
        "field_label": "Image",
        "document_ids": ["p1", "p2"],
        "count": 2,
        "is_global": false
    }
]
```

This endpoint performs the full back-reference scan, so it's heavier than the ref count check. It's designed for on-demand use (e.g., the "Show details" button).

## Migration

When upgrading to a version with reference counting, the `_ref_count` column is automatically added to all collection and global tables. A one-time backfill migration computes the initial counts from existing relationship data. This runs automatically on first startup and is gated by a `_crap_meta` flag so it only runs once.
