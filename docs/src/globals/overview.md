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
        { name = "site_name", type = "text", required = true, default_value = "My Site" },
        { name = "tagline", type = "text" },
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
| `hooks` | table | `{}` | Same lifecycle hooks as collections |
| `access` | table | `{}` | Same access control as collections |
| `versions` | boolean or table | `nil` | Versioning config (same as collections) |
| `live` | boolean or string | `nil` | Live update broadcasting (same as collections) |
| `mcp` | table | `{}` | MCP tool config. `{ description = "..." }` |

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
| Versions | Supported | Supported |
| Live updates | Supported | Supported |
| MCP | Supported | Supported |

## Lua API

```lua
-- Get current value
local settings = crap.globals.get("site_settings")
print(settings.site_name)

-- Update
crap.globals.update("site_settings", {
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
