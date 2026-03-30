# Collections

Collections are the core data model in Crap CMS. Each collection maps to a SQLite table and is defined in a Lua file.

## Basics

- One Lua file per collection in the `collections/` directory
- Files are loaded alphabetically at startup
- Each file calls `crap.collections.define(slug, config)`
- The `slug` becomes the table name and URL segment
- Fields, hooks, access control, auth, and uploads are all configured in the definition

## Example

```lua
-- collections/posts.lua
crap.collections.define("posts", {
    labels = {
        singular = "Post",
        plural = "Posts",
    },
    timestamps = true,
    admin = {
        use_as_title = "title",
        default_sort = "-created_at",
        list_searchable_fields = { "title", "slug" },
    },
    fields = {
        crap.fields.text({ name = "title", required = true }),
        crap.fields.text({ name = "slug", required = true, unique = true }),
        crap.fields.select({
            name = "status",
            default_value = "draft",
            options = {
                { label = "Draft", value = "draft" },
                { label = "Published", value = "published" },
            },
        }),
        crap.fields.richtext({ name = "content" }),
    },
    hooks = {
        before_change = { "hooks.posts.auto_slug" },
    },
    access = {
        read   = "hooks.access.public_read",
        create = "hooks.access.authenticated",
        delete = "hooks.access.admin_only",
    },
})
```

## System Fields

Every collection automatically has these columns (not in your field definitions):

| Field | Type | Description |
|-------|------|-------------|
| `id` | TEXT PRIMARY KEY | Auto-generated nanoid |
| `created_at` | TEXT | ISO 8601 timestamp (if `timestamps = true`) |
| `updated_at` | TEXT | ISO 8601 timestamp (if `timestamps = true`) |

Auth collections also get a hidden `_password_hash` TEXT column.

Versioned collections with `drafts = true` also get:

| Field | Type | Description |
|-------|------|-------------|
| `_status` | TEXT | `"published"` or `"draft"` (auto-managed) |

Versioned collections also get a companion `_versions_{slug}` table that stores JSON snapshots of every save. See [Versions & Drafts](versions.md).

## Schema Sync

On startup, Crap CMS compares each Lua definition against the existing SQLite table:

- **Missing table** — creates it with all defined columns
- **Missing columns** — adds them via `ALTER TABLE`
- **Removed columns** — logged as a warning (SQLite doesn't easily drop columns)
- **Type changes** — not automatically migrated (manual intervention needed)
