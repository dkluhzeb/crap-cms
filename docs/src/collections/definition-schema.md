# Collection Definition Schema

Full reference for every property accepted by `crap.collections.define(slug, config)`.

## Top-Level Properties

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `labels` | table | `{}` | Display names for the admin UI |
| `labels.singular` | string | slug | Singular name (e.g., "Post") |
| `labels.plural` | string | slug | Plural name (e.g., "Posts") |
| `timestamps` | boolean | `true` | Auto-manage `created_at` and `updated_at` |
| `fields` | FieldDefinition[] | `{}` | Field definitions (see [Fields](../fields/overview.md)) |
| `admin` | table | `{}` | Admin UI options |
| `hooks` | table | `{}` | Lifecycle hook references |
| `auth` | boolean or table | `nil` | Authentication config (see [Auth Collections](../authentication/auth-collections.md)) |
| `upload` | boolean or table | `nil` | Upload config (see [Uploads](../uploads/overview.md)) |
| `access` | table | `{}` | Access control function refs |

## `admin`

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `use_as_title` | string | `nil` | Field name to display as the row label in admin lists |
| `default_sort` | string | `nil` | Default sort field. Prefix with `-` for descending (e.g., `"-created_at"`) |
| `hidden` | boolean | `false` | Hide this collection from the admin sidebar |
| `list_searchable_fields` | string[] | `{}` | Fields to search when using the admin list search bar |

## `hooks`

All hook values are arrays of string references in `module.function` format.

| Property | Type | Description |
|----------|------|-------------|
| `before_validate` | string[] | Runs before field validation. Has CRUD access. |
| `before_change` | string[] | Runs after validation, before write. Has CRUD access. |
| `after_change` | string[] | Runs after create/update. No CRUD access (fire-and-forget). |
| `before_read` | string[] | Runs before returning read results. No CRUD access. |
| `after_read` | string[] | Runs after read, before response. No CRUD access. |
| `before_delete` | string[] | Runs before delete. Has CRUD access. |
| `after_delete` | string[] | Runs after delete. No CRUD access (fire-and-forget). |

See [Hooks](../hooks/overview.md) for full details.

## `auth`

Set to `true` for defaults, or provide a config table:

```lua
-- Simple
auth = true

-- With options
auth = {
    token_expiry = 3600,
    disable_local = false,
    strategies = {
        { name = "api-key", authenticate = "hooks.auth.api_key_check" },
    },
}
```

See [Auth Collections](../authentication/auth-collections.md) for the full schema.

## `upload`

Set to `true` for defaults, or provide a config table:

```lua
-- Simple
upload = true

-- With options
upload = {
    mime_types = { "image/*" },
    max_file_size = 10485760,
    image_sizes = {
        { name = "thumbnail", width = 300, height = 300, fit = "cover" },
    },
    format_options = {
        webp = { quality = 80 },
    },
}
```

See [Uploads](../uploads/overview.md) for the full schema.

## `access`

| Property | Type | Description |
|----------|------|-------------|
| `read` | string | Lua function ref for read access. |
| `create` | string | Lua function ref for create access. |
| `update` | string | Lua function ref for update access. |
| `delete` | string | Lua function ref for delete access. |

If a property is omitted, that operation is allowed for everyone.

See [Access Control](../access-control/overview.md) for full details.

## Complete Example

```lua
crap.collections.define("posts", {
    labels = {
        singular = "Post",
        plural = "Posts",
    },
    timestamps = true,
    admin = {
        use_as_title = "title",
        default_sort = "-created_at",
        hidden = false,
        list_searchable_fields = { "title", "slug", "content" },
    },
    fields = {
        {
            name = "title",
            type = "text",
            required = true,
            hooks = {
                before_validate = { "hooks.posts.trim_title" },
            },
        },
        {
            name = "slug",
            type = "text",
            required = true,
            unique = true,
        },
        {
            name = "status",
            type = "select",
            required = true,
            default_value = "draft",
            options = {
                { label = "Draft", value = "draft" },
                { label = "Published", value = "published" },
                { label = "Archived", value = "archived" },
            },
        },
        { name = "content", type = "richtext" },
        {
            name = "tags",
            type = "relationship",
            relationship = { collection = "tags", has_many = true },
        },
    },
    hooks = {
        before_change = { "hooks.posts.auto_slug" },
    },
    access = {
        read   = "hooks.access.public_read",
        create = "hooks.access.authenticated",
        update = "hooks.access.authenticated",
        delete = "hooks.access.admin_only",
    },
})
```
