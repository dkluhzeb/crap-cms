# Collection-Level Access Control

Collection-level access controls who can perform CRUD operations on a collection.

## Configuration

```lua
crap.collections.define("posts", {
    access = {
        read   = "hooks.access.public_read",
        create = "hooks.access.authenticated",
        update = "hooks.access.authenticated",
        trash  = "hooks.access.authenticated",
        delete = "hooks.access.admin_only",
    },
    -- ...
})
```

Each property is a Lua function ref (string) or `nil` (no restriction). The
value must be a string — a non-string (e.g. a function value or boolean) is a
hard error at load time, not a silently dropped rule. The same applies to
field-level `access` and global `access`.

| Property | Controls | Fallback |
|----------|----------|----------|
| `read` | `Find` / `FindByID` / `count` / `search` of **published** content | — (governed by `default_deny`) |
| `create` | `Create` operation | — |
| `update` | `Update` operation | — |
| `trash` | Soft-delete (move to trash) and restore. Only relevant when `soft_delete = true`. | `update` |
| `delete` | Permanent deletion, empty trash. For collections without `soft_delete`, this is the only delete permission. | — |
| `draft` | Reading **unpublished (draft)** content — any read that opts into drafts (`draft = true` / `use_draft` / `include_drafts`). Only relevant when `drafts` is enabled. | `update` |
| `versions` | Reading **version history** (`list_versions` / reading a version snapshot). A *toggle* — set it to deny history access entirely. Only relevant when `versions` is enabled. | **allow** |

> **Note:** When `soft_delete = true`, `trash` and `delete` are separate permissions.
> `trash` controls the reversible action (low privilege), `delete` controls the
> destructive action (high privilege). If `trash` is not set, it falls back to
> `update`. If `delete` is not set, permanent deletion is restricted to the
> auto-purge scheduler. See [Soft Deletes](../collections/soft-deletes.md).

> **Drafts require edit-level access.** A plain `read` rule gates **published**
> content only. Pulling unpublished content (via the `draft` opt-in —
> `draft = true` / `use_draft` / `include_drafts`) is gated by `draft`, which
> **falls back to `update`** when unset — so by default only users who can edit
> can preview drafts, and a public `read` rule never exposes unpublished content.
> Set `draft` explicitly to gate previews behind a different policy than editing.
> The same rule applies to globals. See [Versions & Drafts](../collections/versions.md).
>
> Operators never write `_status` themselves — each access key scopes its own
> status, and a user-supplied `_status` filter is rejected as a system column.
> Opting into drafts is always the typed `draft` flag, never a raw filter.

> **Version history is a separate toggle.** `versions` controls *whether* a user
> may see version history at all. Unlike `trash`/`draft` it does **not** fall
> back to `update` — unset means **allow**, so by default anyone who can read the
> document can browse its history. *Which* snapshots they see is still the
> composite of `read` (published snapshots) and `draft` (draft snapshots): a
> reader without draft access sees only published snapshots. `versions` is a pure
> toggle — return `true`/`false`; a filter table is a configuration error
> (row-level scoping belongs on `read`). Restoring a version requires **both**
> `update` (it writes the live document) **and** `versions` (it resurrects
> historical content) — so denying `versions` walls off historical content
> entirely, not just its listing. Only relevant when `versions = true`.

## Writing Access Functions

Access functions live in Lua modules under the config directory:

```lua
-- hooks/access.lua
local M = {}

-- Allow anyone (including anonymous)
function M.public_read(ctx)
    return true
end

-- Require any authenticated user
function M.authenticated(ctx)
    return ctx.user ~= nil
end

-- Require admin role
function M.admin_only(ctx)
    return ctx.user ~= nil and ctx.user.role == "admin"
end

-- Allow users to only read their own documents
function M.own_only(ctx)
    if ctx.user == nil then return false end
    if ctx.user.role == "admin" then return true end
    return { created_by = ctx.user.id }  -- filter constraint
end

return M
```

## Return Values

| Return Value | Effect |
|-------------|--------|
| `true` | Operation is allowed |
| `false` or `nil` | Operation is denied (403/permission error) |
| table (filter) | Allowed, **scoped** by the filter — the exact meaning depends on the key (see below) |

A returned filter table is interpreted differently per access key — it is never
a no-op:

- **`read` / `draft` / `trash`** — scopes *which rows the view returns*, e.g.
  `return { created_by = ctx.user.id }` lists only the caller's own documents.
  See [Filter Constraints](filter-constraints.md).
- **`update` / `delete` / `undelete` / `unpublish`** — **enforced as a row
  guard**: the write proceeds only if the target row matches the filter,
  otherwise it is denied. This is how you express "users may only update/delete
  rows where `created_by = me`" — a real ownership-scoping feature.
- **`create`** — a filter table is a **configuration error**: there is no target
  row yet, so gate creates with `true`/`false` based on `ctx.data`.
- **`versions`** — a filter table is a **configuration error**: it is a boolean
  toggle; per-row scoping of history belongs on `read`/`draft`.

## Enforcement Points

- **Admin UI** — middleware checks access before rendering pages
- **gRPC API** — service checks access before executing operations
- Access is checked once, before the operation begins
