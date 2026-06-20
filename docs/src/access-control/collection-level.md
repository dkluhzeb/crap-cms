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
| `versions` | Reading **version history** (`list_versions` / reading a version snapshot). When set, a *toggle* — set it to restrict history access behind a stricter policy. Only relevant when `versions` is enabled. | `update` |
| `unlock` | The account **lock / unlock** operations (`LockAccount` / `UnlockAccount`) on an **auth** collection. Set it to grant a *narrower* privilege than full edit — e.g. a moderator who may block logins but not edit user fields. A filter table scopes *which* users the caller may (un)lock. Only relevant on auth collections. | `update` |
| `admin` | Whether the collection/global is visible/usable in the **admin UI** (nav entry + its admin routes). A boolean rule (filter table = config error). **Permissive default** (visible when unset) — only ever *further* restricts admin-UI access beyond `read`. Valid on collections **and** globals. | **allow** (visible) |
| `mcp` | Whether the collection is exposed to the **MCP** surface (tools + introspection + resources). A boolean rule (filter table = config error — MCP has a shared key, no per-user identity). **Permissive default** (exposed when unset, if MCP is enabled). Valid on collections **and** globals. | **allow** (exposed) |

> **Account actions on auth collections.** `LockAccount` / `UnlockAccount` are
> gated by `unlock` (falling back to `update`), so by default blocking a user's
> login requires the same edit access the admin UI enforces — set `unlock`
> explicitly to carve out a narrower moderator role. `VerifyAccount` /
> `UnverifyAccount` (forcing a user's email-verified state) are gated by `update`,
> since they set a property on the user document. None of these are reachable by a
> merely-authenticated caller: every one requires access to the target user.

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

> **Version history follows edit access by default.** Like `draft`/`trash`,
> `versions` falls back to **`update`** when unset: by default, only someone who
> can edit a document may browse its history — a published-only reader cannot. A
> row-filtered `update` (e.g. `{ author = ctx.user.id }`) scopes history to the
> documents the caller may edit. This means enabling the `versions` feature works
> out of the box with your existing `update` rule — no separate access rule
> needed. Set `versions` explicitly to restrict history *further* than editing
> (e.g. only admins inspect history even though editors can edit); when set it is
> a pure toggle — return `true`/`false`, a filter table is a configuration error
> (row-level scoping belongs on `read`/`update`). *Which* snapshots are returned
> is independently the composite of `read` (published snapshots) and `draft`
> (draft snapshots), so content is bounded even where the timeline is visible.
> Restoring a version requires **both** `update` (it writes the live document)
> **and** the `versions` toggle. Only relevant when `versions = true`.

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
- **`update` / `delete` / `undelete` / `unpublish` / `unlock`** — **enforced as a
  row guard**: the operation proceeds only if the target row matches the filter,
  otherwise it is denied. This is how you express "users may only update/delete
  rows where `created_by = me`", or "a moderator may only unlock users in their
  own org (`{ org = ctx.user.org }`)" — a real ownership-scoping feature.
- **`create`** — a filter table is a **configuration error**: there is no target
  row yet, so gate creates with `true`/`false` based on `ctx.data`.
- **`versions`** — a filter table is a **configuration error**: it is a boolean
  toggle; per-row scoping of history belongs on `read`/`draft`.

## Enforcement Points

- **Admin UI** — middleware checks access before rendering pages
- **gRPC API** — service checks access before executing operations
- Access is checked once, before the operation begins
