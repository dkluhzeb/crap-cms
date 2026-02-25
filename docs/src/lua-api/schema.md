# crap.schema

Schema introspection API. Provides read-only access to collection and global definitions loaded from Lua files. Available everywhere the `crap` global is accessible.

## crap.schema.get_collection(slug)

Get a collection's full schema definition. Returns a table or `nil` if not found.

```lua
local schema = crap.schema.get_collection("posts")
if schema then
    print(schema.slug)           -- "posts"
    print(schema.timestamps)     -- true
    print(schema.has_auth)       -- false
    print(#schema.fields)        -- number of fields
    for _, field in ipairs(schema.fields) do
        print(field.name, field.type, field.required)
    end
end
```

### Return Value

| Field | Type | Description |
|-------|------|-------------|
| `slug` | string | Collection slug. |
| `labels` | table | `{ singular?, plural? }` display names. |
| `timestamps` | boolean | Whether created_at/updated_at are enabled. |
| `has_auth` | boolean | Whether authentication is enabled. |
| `has_upload` | boolean | Whether file uploads are enabled. |
| `has_versions` | boolean | Whether versioning is enabled. |
| `fields` | table[] | Array of field definitions (see below). |

### Field Schema

Each field table contains:

| Field | Type | Description |
|-------|------|-------------|
| `name` | string | Field name. |
| `type` | string | Field type (text, number, relationship, etc.). |
| `required` | boolean | Whether the field is required. |
| `localized` | boolean | Whether the field has per-locale values. |
| `unique` | boolean | Whether the field has a unique constraint. |
| `relationship` | table? | `{ collection, has_many }` for relationship fields. |
| `options` | table[]? | `{ label, value }` for select fields. |
| `fields` | table[]? | Sub-fields for array/group types (recursive). |
| `blocks` | table[]? | Block definitions for blocks type. |

## crap.schema.get_global(slug)

Get a global's schema definition. Same return shape as `get_collection`.

```lua
local schema = crap.schema.get_global("site_settings")
```

## crap.schema.list_collections()

List all registered collections with their slugs and labels.

```lua
local collections = crap.schema.list_collections()
for _, c in ipairs(collections) do
    print(c.slug, c.labels.singular)
end
```

## crap.schema.list_globals()

List all registered globals with their slugs and labels.

```lua
local globals = crap.schema.list_globals()
for _, g in ipairs(globals) do
    print(g.slug, g.labels.singular)
end
```
