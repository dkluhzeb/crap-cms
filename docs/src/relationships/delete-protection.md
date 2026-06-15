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
| Has-many inside a block | JSON array in blocks table | Yes |
| Relationships nested in a group (inside an array or block) | Recursed into nested JSON | Yes |
| Global outgoing refs | Global table columns | Yes |

Counting recurses through **every** nesting combination — a relationship
inside a group inside an array, a group inside a block, a has-many list
inside a block, and so on are all tracked at any depth.

## Scope

Delete protection applies to **all collections**, not just uploads. Any document referenced by another document is protected.

## Soft Delete Interaction

Soft-deleting a document does **not** adjust ref counts. The outgoing references remain counted because:

- Soft-deleted documents can be restored, so their references should remain tracked
- Trashed documents still "own" their references in the database

Only **hard deletion** (permanent) decrements ref counts on the targets.

The ref count check only blocks **deletion of a target**: a document can always be soft-deleted regardless of how many others reference it.

When a referenced document is soft-deleted, it is **omitted** from relationship population on read — it does not appear as an ID string or as a populated object. Has-one fields resolve to `null`; has-many fields have the soft-deleted entry dropped from the array. Restore the document to make it appear again.

## Admin UI

The delete confirmation page shows a warning when a document has `_ref_count > 0`:

> **This document is referenced by other content.**
> [Show details]

The warning is **non-quantified** — it never renders the raw `_ref_count` as a
number. The raw count includes references from collections the current user
cannot read, so exposing it would leak the existence of inaccessible content.
The block decision itself still uses the raw count (see
[Access filtering](#access-filtering) below).

Clicking **Show details** lazy-loads the **access-filtered** list of referring
documents, fields, and counts via the back-references API endpoint.

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

-- Bulk delete: skips referenced documents and reports the count
local result = crap.collections.media.delete_many({
    where = { status = { equals = "unused" } }
})
-- result.deleted = documents actually deleted
-- result.skipped = documents skipped due to outstanding references
```

### Force Hard Delete

The `forceHardDelete` option bypasses the ref count check. This is used internally for **Empty Trash** operations and can be used in Lua hooks:

```lua
crap.collections.media.delete("m1", {
    forceHardDelete = true  -- skips ref count check
})
```

## Back-References API

To see which documents reference a target, use the back-references endpoint:

```
GET /admin/collections/{slug}/{id}/back-references
```

Returns a JSON object:

```json
{
    "references": [
        {
            "owner_slug": "posts",
            "owner_label": "Posts",
            "field_name": "image",
            "field_label": "Image",
            "document_ids": ["p1", "p2"],
            "count": 2,
            "is_global": false
        }
    ],
    "has_inaccessible": false
}
```

This endpoint performs the full back-reference scan, so it's heavier than the ref count check. It's designed for on-demand use (e.g., the "Show details" button).

## Access filtering

The two questions delete protection answers are gated differently:

- **"Can this document be deleted?"** — uses the raw `_ref_count` and is
  **visibility-blind**. It is a system-integrity invariant: a document with
  *any* incoming reference is blocked, even references the current user cannot
  see. This is what keeps the database free of orphaned references regardless
  of who is logged in.

- **"Which documents reference it?"** — the back-references list is **fully
  access-filtered**. Access is resolved once per referring collection/global,
  through the same view scope that gates normal reads: a referrer appears only
  if the user could read it via *some* view (published, draft, or trash). For a
  collection access rule that returns row constraints, those constraints are
  folded into the scan so only matching referrers are listed.

When the access-filtered list is **smaller** than the raw count — some referrers
were dropped because the user cannot access them — the response sets
`has_inaccessible: true`. The UI then shows a non-quantified note:

> Also referenced by documents you don't have access to.

The hidden count is never revealed — only that something exists. This resolves
the "blocked but nothing visible" case: a user can be correctly prevented from
deleting a document while seeing *why* without learning what the inaccessible
referrers are.

## Migration

When upgrading to a version with reference counting, the `_ref_count` column is automatically added to all collection and global tables. A one-time backfill migration computes the initial counts from existing relationship data. This runs automatically on first startup and is gated by a `_crap_meta` flag so it only runs once.
