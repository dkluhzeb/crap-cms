# Versions & Drafts

Crap CMS supports document versioning with an optional draft/publish workflow.

## Enabling Versions

Add `versions` to your collection definition:

```lua
-- Simple: enables versions with drafts
crap.collections.define("articles", {
    versions = true,
    fields = { ... },
})

-- With options
crap.collections.define("articles", {
    versions = {
        drafts = true,
        max_versions = 20,
    },
    fields = { ... },
})
```

### Config Properties

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `drafts` | boolean | `true` | Enable draft/publish workflow with `_status` field |
| `max_versions` | integer | `0` | Max versions per document. `0` = unlimited. Oldest versions are pruned first. |

Setting `versions = true` is equivalent to `{ drafts = true, max_versions = 0 }`.

Setting `versions = false` or omitting it disables versioning entirely.

## How It Works

When versioning is enabled, every create and update operation saves a **JSON snapshot** of the document to a `_versions_{slug}` table. This provides a full audit trail with the ability to restore any previous version.

### Database Changes

Versioned collections get an additional table:

```sql
_versions_articles (
    id TEXT PRIMARY KEY,
    _parent TEXT NOT NULL REFERENCES articles(id) ON DELETE CASCADE,
    _version INTEGER NOT NULL,
    _status TEXT NOT NULL,        -- "published" or "draft"
    _latest INTEGER NOT NULL DEFAULT 0,  -- 1 for the most recent version
    snapshot TEXT NOT NULL,               -- full JSON snapshot
    created_at TEXT DEFAULT (datetime('now')),
    updated_at TEXT DEFAULT (datetime('now'))
)
```

When `drafts = true`, the main table also gets a `_status` column (`TEXT NOT NULL DEFAULT 'published'`).

## Draft/Publish Workflow

When `drafts = true`, documents have a `_status` field that is either `"published"` or `"draft"`.

### Creating Documents

| Action | Result |
|--------|--------|
| Create (publish) | Document inserted with `_status = 'published'` + version snapshot |
| Create (draft) | Document inserted with `_status = 'draft'` + version snapshot |

### Updating Documents

| Action | Result |
|--------|--------|
| Update (publish) | Main table updated, `_status = 'published'` + new version snapshot |
| Update (draft) | **Version-only save** — main table is NOT modified, only a new draft version snapshot is created |
| Unpublish | `_status` set to `'draft'` + new version snapshot |

The version-only draft save is key: it lets authors iterate on changes without affecting the published version. The main table always reflects the last published state.

### Reading Documents

| API Call | Default Behavior |
|----------|-----------------|
| `Find` | Returns only `_status = 'published'` documents |
| `Find` with `draft = true` | Returns all documents (published + draft) |
| `FindByID` | Returns the main table document (published version) |
| `FindByID` with `draft = true` | Returns the **latest version snapshot** (may be a newer draft) |

### Validation

**Required field validation is skipped for draft saves.** This lets authors save incomplete work. Validation is enforced when publishing (`draft = false`).

## gRPC API

### Draft Parameter

The `draft` parameter is available on these RPCs:

```protobuf
// Create a draft
CreateRequest { collection, data, draft: true }

// Draft update (version-only, main table unchanged)
UpdateRequest { collection, id, data, draft: true }

// Find all documents including drafts
FindRequest { collection, draft: true }

// Get the latest version (may be a newer draft)
FindByIDRequest { collection, id, draft: true }
```

### ListVersions

List version history for a document:

```bash
grpcurl -plaintext -d '{
    "collection": "articles",
    "id": "abc123",
    "limit": "10"
}' localhost:50051 crap.ContentAPI/ListVersions
```

Response:

```json
{
    "versions": [
        { "id": "v1", "version": 3, "status": "draft", "latest": true, "created_at": "..." },
        { "id": "v2", "version": 2, "status": "published", "latest": false, "created_at": "..." },
        { "id": "v3", "version": 1, "status": "published", "latest": false, "created_at": "..." }
    ]
}
```

### RestoreVersion

Restore a previous version, writing its snapshot data back to the main table:

```bash
grpcurl -plaintext -d '{
    "collection": "articles",
    "document_id": "abc123",
    "version_id": "v3"
}' localhost:50051 crap.ContentAPI/RestoreVersion
```

This overwrites the main table with the snapshot data, sets `_status` to `"published"`, and creates a new version entry for the restore.

## Lua API

The `draft` option is available on `create` and `update`:

```lua
-- Create as draft
local doc = crap.collections.articles.create({
    title = "Work in progress",
}, { draft = true })

-- Draft update (version-only save)
crap.collections.articles.update(doc.id, {
    title = "Still editing...",
}, { draft = true })

-- Publish
crap.collections.articles.update(doc.id, {
    title = "Final Title",
})  -- draft defaults to false
```

## Admin UI

### Buttons

When drafts are enabled, the edit form shows context-aware buttons:

| Document State | Primary Button | Secondary Button | Extra |
|---------------|---------------|-----------------|-------|
| Create (new) | Publish | Save as Draft | |
| Editing (draft) | Publish | Save Draft | |
| Editing (published) | Update | Save Draft | Unpublish |

### Status Badge

A status badge (`published` or `draft`) appears in the document meta panel and in the collection list view.

### Version History

The edit sidebar shows a "Version History" panel listing recent versions with:

- Version number
- Status badge (published/draft)
- Timestamp
- **Restore** button (for non-latest versions)

Clicking Restore writes the snapshot data back to the main table and redirects to the edit form.

## Access Control

**Reading** drafts and version history has dedicated access keys (see
[Collection-Level Access](../access-control/collection-level.md)):

- `access.draft` — who may read **unpublished (draft)** content. Falls back to
  `access.update` when unset, so by default only editors preview drafts and a
  plain `read` rule exposes published content only.
- `access.versions` — a toggle for who may read **version history**. Defaults to
  **allow**; *which* snapshots are visible still follows `read` (published
  snapshots) and `draft` (draft snapshots).

```lua
access = {
    read  = "hooks.access.public_read",   -- published content
    draft = "hooks.access.editors",       -- preview unpublished content
    -- versions defaults to allow
}
```

**Writing** is status-agnostic — `create`/`update` gate the write regardless of
publish state. If you want finer-grained *publish* control (e.g. editors may
save drafts, but only admins may publish), inspect the incoming `data._status`
in your **write** rule — this is a write-side concern, separate from the
read-side `access.draft` key above:

```lua
function hooks.access.publish_control(ctx)  -- used as access.update / access.create
    if ctx.data and ctx.data._status == "draft" then
        return ctx.user ~= nil          -- any authenticated user can save a draft
    end
    return ctx.user and ctx.user.role == "admin"  -- only admins can publish
end
```

## Versions Without Drafts

You can enable version history without the draft/publish workflow:

```lua
versions = {
    drafts = false,
    max_versions = 50,
}
```

This creates version snapshots on every save but does not add a `_status` column, does not filter by publish state, and does not show draft/publish buttons in the admin UI. Useful for pure audit trails.

## Example

```lua
crap.collections.define("articles", {
    labels = { singular = "Article", plural = "Articles" },
    timestamps = true,
    versions = {
        drafts = true,
        max_versions = 20,
    },
    admin = {
        use_as_title = "title",
        default_sort = "-created_at",
    },
    fields = {
        crap.fields.text({ name = "title", required = true }),
        crap.fields.text({ name = "slug", required = true, unique = true }),
        crap.fields.textarea({ name = "summary" }),
        crap.fields.richtext({ name = "body" }),
    },
    access = {
        read   = "hooks.access.public_read",
        create = "hooks.access.authenticated",
        update = "hooks.access.authenticated",
        delete = "hooks.access.admin_only",
    },
})
```
