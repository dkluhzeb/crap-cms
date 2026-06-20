# Collection Definition Schema

Full reference for every property accepted by `crap.collections.define(slug, config)`.

> **Unknown keys are rejected.** The collection config table — and its nested
> field/`admin` tables — only accept the documented keys. A typo like
> `timestamp` (for `timestamps`) or `version` (for `versions`) fails loudly at
> load time rather than being silently ignored.

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
| `versions` | boolean or table | `nil` | Versioning and drafts config (see [Versions & Drafts](versions.md)) |
| `soft_delete` | boolean | `false` | Enable soft deletes (see [Soft Deletes](soft-deletes.md)) |
| `soft_delete_retention` | string | `nil` | Auto-purge retention period (e.g., `"30d"`). Requires `soft_delete = true`. |
| `live` | boolean or string | `nil` | Live update broadcasting (see [Live Updates](../live-updates/overview.md)) |
| `mcp` | table | `{}` | MCP tool config. `description` is appended to every generated tool; `operations` is an optional `{ op = "..." }` map of per-operation description overrides (see [MCP overview](../mcp/overview.md#per-operation-descriptions)). |
| `indexes` | IndexDefinition[] | `{}` | Compound indexes (see [Indexes](#indexes) below) |

## `admin`

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `use_as_title` | string | `nil` | Field name to display as the row label in admin lists |
| `default_sort` | string | `nil` | Default sort field. Prefix with `-` for descending (e.g., `"-created_at"`) |
| `hidden` | boolean | `false` | Hide this collection from the admin sidebar |
| `list_searchable_fields` | string[] | `{}` | Fields to search when using the admin list search bar |
| `list_columns` | string[] | `{}` | Default columns shown in the admin list view, in order. Entries are field names or the meta columns `created_at` / `updated_at` / `_status`. Empty = built-in default (`_status` if the collection has drafts, plus `created_at`). A per-user column selection overrides it. |

## `hooks`

All hook values are arrays of string references in `module.function` format.

| Property | Type | Description |
|----------|------|-------------|
| `before_validate` | string[] | Runs before field validation. Has CRUD access. |
| `before_change` | string[] | Runs after validation, before write. Has CRUD access. |
| `after_change` | string[] | Runs after create/update (inside transaction). Has CRUD access. Errors roll back. |
| `before_read` | string[] | Runs before returning read results. No CRUD access. |
| `after_read` | string[] | Runs after read, before response. No CRUD access. |
| `before_delete` | string[] | Runs before delete. Has CRUD access. |
| `after_delete` | string[] | Runs after delete (inside transaction). Has CRUD access. Errors roll back. |
| `before_broadcast` | string[] | Runs after commit, before broadcast. No CRUD access. See [Live Updates](../live-updates/hooks.md). |

See [Hooks](../hooks/overview.md) for full details.

## `auth`

Set to `true` for defaults, or provide a config table:

```lua
-- Simple
auth = true

-- With options
auth = {
    enabled = true,
    token_expiry = 3600,
    methods = crap.auth.with_defaults({
        { type = "strategy",
          name = "api-key",
          authenticate = "hooks.auth.api_key_check",
          activates_on = { header = "x-api-key" },
          surfaces = { "grpc" } },
    }),
}
```

See [Auth Collections](../authentication/auth-collections.md) and [Auth Methods](../authentication/auth-methods.md) for the full schema.

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
Each content view has its own key (all the same shape — a Lua function ref returning `true`, `false`, or a row-filter table). A read returns the **union** of the views the caller is allowed to see.

| Property | Type | Description |
|----------|------|-------------|
| `read` | string | Read access to **published** documents. |
| `create` | string | Lua function ref for create access. |
| `update` | string | Lua function ref for update access. |
| `delete` | string | Lua function ref for delete access. |
| `draft` | string | Read access to **unpublished (draft)** documents. Falls back to `update` when omitted. Only relevant with `versions` drafts enabled. |
| `trash` | string | Read access to **soft-deleted (trash)** documents, plus restore. Falls back to `update` when omitted, so most collections don't set it explicitly. |
| `versions` | string | Toggles **version-history** visibility (a boolean rule, not a row filter). Falls back to `update` when omitted (history follows edit access); snapshot contents are still bounded by `read`/`draft`. |
| `unlock` | string | Gates the account **lock/unlock** operations on an **auth** collection. Falls back to `update` when omitted. A row-filter table scopes which users the caller may (un)lock. Only meaningful on auth collections; rejected on globals. (Verify/unverify are gated by `update`.) |

When `read`/`create`/`update`/`delete` is omitted, the behavior depends on `[access] default_deny` in `crap.toml`: with the default `default_deny = true`, that operation is **denied** for everyone; with `default_deny = false`, it is **allowed** for everyone. `draft`, `trash`, `versions`, and `unlock` instead fall back to `update` (see the per-key notes above).

See [Access Control](../access-control/overview.md) for full details.

## `versions`

Set to `true` for defaults (drafts enabled, unlimited versions), or provide a config table:

```lua
-- Simple: versions with drafts
versions = true

-- With options
versions = {
    drafts = true,
    max_versions = 20,
}

-- Versions without drafts (pure audit trail)
versions = {
    drafts = false,
    max_versions = 50,
}
```

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `drafts` | boolean | `true` | Enable draft/publish workflow with `_status` field |
| `max_versions` | integer | `0` | Max versions per document. `0` = unlimited. |

See [Versions & Drafts](versions.md) for the full workflow.

## Indexes

### Field-Level Indexes

Set `index = true` on a field to create a B-tree index on its column. This speeds up queries that filter or sort on that field. Unique fields are already indexed by SQLite, so `index = true` is skipped when `unique = true`.

```lua
crap.fields.text({ name = "status", index = true }),
crap.fields.date({ name = "published_at", index = true }),
```

For localized fields, one index is created per locale column (e.g., `idx_posts_title__en`, `idx_posts_title__de`).

### Compound Indexes

Use the top-level `indexes` array for multi-column indexes:

```lua
indexes = {
    { fields = { "status", "created_at" } },
    { fields = { "category", "slug" }, unique = true },
}
```

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `fields` | string[] | **required** | Column names to include in the index. |
| `unique` | boolean | `false` | Create a UNIQUE index. |

Indexes are synced idempotently on startup: missing indexes are created with `CREATE INDEX IF NOT EXISTS`, and stale indexes (from removed fields or changed definitions) are dropped. Only indexes with the `idx_{collection}_` naming prefix are managed — external indexes are left untouched.

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
        list_columns = { "title", "_status", "created_at" },
    },
    fields = {
        crap.fields.text({
            name = "title",
            required = true,
            hooks = {
                before_validate = { "hooks.posts.trim_title" },
            },
        }),
        crap.fields.text({
            name = "slug",
            required = true,
            unique = true,
        }),
        crap.fields.select({
            name = "status",
            required = true,
            default_value = "draft",
            options = {
                { label = "Draft", value = "draft" },
                { label = "Published", value = "published" },
                { label = "Archived", value = "archived" },
            },
        }),
        crap.fields.richtext({ name = "content" }),
        crap.fields.relationship({
            name = "tags",
            relationship = { collection = "tags", has_many = true },
        }),
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
