# Globals

Globals are single-document collections for site-wide settings. Each global stores exactly one row.

## Definition

Define globals in `globals/*.lua` using `crap.globals.define()`:

```lua
-- globals/site_settings.lua
crap.globals.define("site_settings", {
    labels = {
        singular = "Site Settings",
    },
    fields = {
        crap.fields.text({ name = "site_name", required = true, default_value = "My Site" }),
        crap.fields.text({ name = "tagline" }),
    },
})
```

## Config Properties

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `labels` | table | `{}` | Display names |
| `labels.singular` | string | slug | Singular name (e.g., "Site Settings") |
| `labels.plural` | string | slug | Plural name |
| `fields` | FieldDefinition[] | `{}` | Field definitions |
| `hooks` | table | `{}` | The collection lifecycle hooks except `before_delete` / `after_delete` — a global is never deleted, so those are rejected at load |
| `access` | table | `{}` | Access rules. Globals honor `read`, `draft`, `update`, and the `versions` toggle — there is no `create`/`delete`/`trash` (a global has one row), and global access functions must return `true`/`false`, not a filter table. See [Access Control](../access-control/overview.md). |
| `versions` | boolean or table | `nil` | Versioning config (same as collections) |
| `live` | boolean or string | `nil` | Live update broadcasting (same as collections) |
| `mcp` | table | `{}` | MCP tool config. `{ description = "..." }` |

With `versions` enabled, publishing a global while a draft is pending takes the pending draft as its base, exactly like a collection document — see [Versions](../collections/versions.md#updating-documents).

Field-level `unique` and `index` are rejected at load on a global's columns (top-level fields and group sub-fields): a global is a single row, so neither could ever apply. (Fields inside an array or blocks field live in their own table and are not affected.)

## Unpublishing

With `versions = { drafts = true }`, a global can be unpublished (admin **Unpublish** button, `crap.globals.<slug>.unpublish()`). Its stored content is kept, but until it is published again every non-draft read — admin API, Lua, gRPC `GetGlobal`, MCP `global_read_*` — returns an **empty** global: every field null, `_status = "draft"`. Reads that opt into drafts (`draft = true`, the admin edit form) still see the content. Publishing again (any published update) makes it visible. Unpublish is refused on a global without drafts.

## Versions by surface

Not every surface exposes a global's version operations:

| Operation | Admin UI | Lua | gRPC | MCP |
|-----------|----------|-----|------|-----|
| Draft save / publish (`update` with `draft`) | Yes | Yes | Yes | Yes |
| Read the draft (`draft = true`) | Yes | Yes | Yes | Yes |
| Unpublish | Yes | Yes | No | No |
| List versions | Yes | No | No | No |
| Restore a version | Yes | No | No | No |

## Database Table

Each global gets a table named `_global_{slug}` with a single row where `id = 'default'`. The row is auto-created on startup.

Globals always have `created_at` and `updated_at` timestamp columns.

## Differences from Collections

| Feature | Collections | Globals |
|---------|-------------|---------|
| Documents | Multiple | Exactly one |
| Table name | `{slug}` | `_global_{slug}` |
| CRUD operations | find, find_by_id, create, update, delete | get, update |
| Timestamps | Optional (`timestamps = true`) | Always enabled |
| Auth / Upload | Supported | Not supported |
| Versions | Supported | Supported (version history and unpublish on fewer surfaces — see [Versions by surface](#versions-by-surface)) |
| Live updates | Supported | Supported |
| MCP | Supported | Supported |

## Lua API

```lua
-- Get current value
local settings = crap.globals.site_settings.get()
print(settings.site_name)

-- Update
crap.globals.site_settings.update({
    site_name = "New Name",
    tagline = "A fresh start",
})
```

## gRPC API

```bash
# Get
grpcurl -plaintext -d '{"slug": "site_settings"}' \
    localhost:50051 crap.ContentAPI/GetGlobal

# Update
grpcurl -plaintext -d '{
    "slug": "site_settings",
    "data": {"site_name": "Updated Site"}
}' localhost:50051 crap.ContentAPI/UpdateGlobal
```
