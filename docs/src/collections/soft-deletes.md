# Soft Deletes

Collections can opt into soft deletes so that deleted documents are moved to trash instead of being permanently removed. Trashed documents can be restored or permanently purged after a configurable retention period.

## Enabling

```lua
crap.collections.define("posts", {
  soft_delete = true,
  soft_delete_retention = "30d",  -- optional: auto-purge after 30 days
  -- ...
})
```

When `soft_delete = true`:
- Deleting a document sets a `_deleted_at` timestamp instead of removing the row
- Soft-deleted documents are excluded from all reads, counts, and search
- Upload files are preserved until the document is permanently purged
- Version history is preserved

## Permissions

Soft deletes introduce a split between trashing (reversible) and permanent deletion (destructive):

| Action | Permission | Fallback | Description |
|--------|-----------|----------|-------------|
| Move to trash | `access.trash` | `access.update` | Soft-delete a document |
| Restore from trash | `access.trash` | `access.update` | Un-delete a trashed document |
| Delete permanently | `access.delete` | *(blocks if not set)* | Permanently remove a document |
| Empty trash | `access.delete` | *(blocks if not set)* | Permanently remove all trashed documents |
| Auto-purge | *(none)* | — | System-level scheduler, always runs |

Example configuration:

```lua
access = {
  read = "access.anyone",
  create = "access.editor_or_above",
  update = "access.editor_or_above",
  trash = "access.editor_or_above",       -- editors can trash and restore
  delete = "access.admin_or_director",    -- only admins can permanently delete
}
```

If `access.trash` is not set, it falls back to `access.update` — any user who can edit a document can also trash it. If `access.delete` is not set, permanent deletion is only possible via the auto-purge scheduler.

## Admin UI

### Trash view

The collection list shows a **Trash** button when `soft_delete` is enabled. Clicking it shows the trash view (`?trash=1`) with:

- **Restore button** — moves the document back to the active list
- **Delete permanently button** — permanently removes the document (only shown when `access.delete` is configured)
- **Empty trash button** — permanently removes all trashed documents (only shown when `access.delete` is configured)

### Delete dialog

The delete button on list rows and the edit sidebar opens a modal dialog:

- **Soft-delete collections**: Shows "Move to trash" (primary) and "Delete permanently" (danger) buttons
- **Hard-delete collections**: Shows only "Delete permanently" button
- "Delete permanently" is hidden when the user lacks `access.delete` permission

## Retention & Auto-Purge

Set `soft_delete_retention` to automatically purge expired documents:

```lua
soft_delete_retention = "30d"   -- purge after 30 days
soft_delete_retention = "7d"    -- purge after 7 days
soft_delete_retention = "90d"   -- purge after 90 days
```

The scheduler runs the purge job periodically. Documents with `_deleted_at` older than the retention period are permanently deleted, including upload file cleanup.

If `soft_delete_retention` is not set, trashed documents persist indefinitely until manually purged via the admin UI or CLI.

Supported formats: `"30d"` (days), `"24h"` (hours), or raw seconds.

## API

### gRPC

```protobuf
// Soft-delete (default for soft_delete collections)
rpc Delete (DeleteRequest) returns (DeleteResponse);

// Force permanent deletion
DeleteRequest { collection: "posts", id: "abc", force_hard_delete: true }

// Undelete from trash
rpc Undelete (UndeleteRequest) returns (UndeleteResponse);
```

The `DeleteResponse` includes a `soft_deleted` boolean indicating whether the deletion was soft or hard.

#### Querying trashed documents

Use `trash = true` on `Find` and `FindByID` to access soft-deleted documents:

```bash
# List all trashed posts (sorted by deletion date, most recent first)
grpcurl -plaintext -d '{
    "collection": "posts",
    "trash": true
}' localhost:50051 crap.ContentAPI/Find

# Find a specific trashed document by ID
grpcurl -plaintext -d '{
    "collection": "posts",
    "id": "abc123",
    "trash": true
}' localhost:50051 crap.ContentAPI/FindByID
```

When `trash = true`:
- Only documents with a `_deleted_at` timestamp are returned
- Default sort is `-_deleted_at` (most recently deleted first)
- `access.trash` is evaluated (falls back to `access.update`, same as
  delete/undelete operations). This means users who can only read but not
  trash/update cannot browse the trash.
- Ignored if the collection does not have `soft_delete = true`

### Lua

```lua
-- Soft delete (moves to trash)
crap.collections.delete("posts", id)

-- Force permanent delete
crap.collections.delete("posts", id, { forceHardDelete = true })

-- Undelete from trash
crap.collections.undelete("posts", id)
```

### MCP

MCP delete tools automatically use soft delete when the collection has it enabled.

### CLI

```bash
# List all trashed documents
crap trash list

# List trashed documents in a specific collection
crap trash list --collection posts

# Restore a document from trash
crap trash restore posts abc123

# Purge all expired documents (respects soft_delete_retention)
crap trash purge

# Purge documents older than 7 days
crap trash purge --older-than 7d

# Dry run — show what would be purged without deleting
crap trash purge --dry-run

# Empty all trash in a collection (requires --confirm)
crap trash empty posts --confirm
```

## Database Schema

When `soft_delete = true`, a `_deleted_at TEXT` column is added to the collection table. The value is `NULL` for active documents and an ISO 8601 timestamp for soft-deleted documents.

All read queries automatically append `AND _deleted_at IS NULL` to exclude trashed documents. The `include_deleted` flag on `FindQuery` overrides this for the trash view.

## Notes

- Soft-deleted documents retain all join table data (arrays, blocks, relationships) — nothing is cascaded
- FTS index entries are removed on soft-delete and re-synced on restore
- Upload files are kept on disk until the document is permanently purged
- Version history is preserved through soft-delete and restore
- Back-reference warnings still appear on the delete confirmation for upload/media collections
