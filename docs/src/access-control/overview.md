# Access Control

Crap CMS provides opt-in access control at both collection and field levels. Access functions are Lua function refs that return one of three values:

- `true` — allowed
- `false` or `nil` — denied
- A filter table (read only) — allowed with query constraints

## Opt-In

By default, collections and globals without explicit access functions **deny all operations** (`default_deny = true`). Every collection must explicitly declare its access rules.

To allow all operations on collections without access functions (e.g., during development), set `default_deny = false` in `[access]` in `crap.toml`.

## Three Levels

1. **Admin panel-level** — `admin.access` in `crap.toml`. A Lua function that gates access to the entire admin UI, checked after login. See [Admin UI](../admin-ui/index.md#access).
2. **Collection-level** — controls who can read, create, update, or delete documents in a collection. See [Collection-Level](collection-level.md).
3. **Field-level** — controls which fields are visible or writable per-user. See [Field-Level](field-level.md).

## Content Views

Reads are gated per **content view**, and each view has its own access key:

| Key | View | Default when unset |
|-----|------|--------------------|
| `read` | published documents | falls back to the collection default (`default_deny`) |
| `draft` | unpublished (draft) documents | `update` |
| `trash` | soft-deleted documents | `update` |
| `versions` | version history | allowed (boolean toggle) |

A read returns the **union** of the views the caller requested *and* is allowed
to see. Because the keys are independent, you can express policies the old
single-`read`-rule model could not:

- **Drafts-only reviewer** — grant `draft`, deny `read`: sees unpublished work, not the live site.
- **Read live but not history** — grant `read`, deny `versions`.

**Reads downgrade; writes deny.** If a caller asks for drafts but lacks `draft`
access, they silently receive published content only — no error. A denied write
(`create`/`update`/`delete`) returns a 403 instead. The reasoning: a read asking
for "more" can safely fall back to "what you're allowed to see", whereas a write
is a single privileged action with no safe fallback.

You never write a `_status` filter yourself — each key scopes its own view. See
[Collection-Level](collection-level.md) for the per-key configuration and
[Filter Constraints](filter-constraints.md) for what a returned filter table may
contain.

> Upgrading from the pre-view model (where drafts were gated by `read`)? See the
> [alpha-10 upgrade notes](../upgrade/alpha-10.md).

## Access Function Context

All access functions receive a context table:

```lua
function M.check(ctx)
    -- ctx.operation  = "create" / "update" / "delete" / "find" / "find_by_id" / …
    -- ctx.collection = the collection (or global) slug this check is for
    -- ctx.user       = full user document (or nil if anonymous)
    -- ctx.id         = document ID (for update/delete/find_by_id)
    -- ctx.data       = incoming data (for create/update)
    -- ctx.locale     = locale this operation targets (when localization is on)
    return true  -- or false, or a filter table
end
```

| Field | Type | Present When | Description |
|-------|------|-------------|-------------|
| `operation` | string | Always | The operation triggering the check: `"create"`, `"update"`, `"delete"`, `"trash"` (soft delete), `"undelete"`, `"unpublish"`, `"restore"`, `"find"`, `"find_by_id"`, `"count"`, `"search"`, `"get"` (global read), `"read"` (admin read-gating), `"subscribe"`, … Lets one shared function gate several operations. |
| `collection` | string | Always | The collection (or global) slug this check is for — so a function reused across collections can tell which one it's gating. |
| `user` | table or nil | Always | Full user document from the auth collection. `nil` if no auth or anonymous. |
| `id` | string or nil | update, delete, find_by_id | Document ID |
| `data` | table or nil | create, update | The **incoming** data being written — *not* the existing stored row. To gate on existing persisted values (e.g. "users may only edit their own rows"), return a **filter table** (e.g. `return { author_id = ctx.user.id }`); the system enforces that the target row matches it. |
| `locale` | string or nil | When localization enabled | The content locale this read/write targets — the requested locale, or the default locale when none was given. `nil` when localization is disabled. Available at both collection and field level. |
| `ui_locale` | string or nil | Admin requests | The operator's admin UI language. Set for admin-originating checks (create/update, global read/update); `nil` for gRPC/REST/internal checks. Distinct from `locale` (the content locale). |

## Per-Locale Access

Because `ctx.locale` is available at both collection and field level, you can
restrict access by locale — e.g. limit a translator to their language, or lock
a field so it's only editable in the default locale.

```lua
-- Restrict updates to a per-user list of allowed locales (a "German translator"
-- may only write German content).
function M.update(ctx)
    if not ctx.user then return false end
    local allowed = ctx.user.locales or {}            -- e.g. {"de"}
    for _, loc in ipairs(allowed) do
        if loc == ctx.locale then return true end
    end
    return false
end

-- Field-level: a field that may only be edited in the default ("en") locale
-- (a common "edit base language only" pattern). Returns true = writable.
function M.title_update(ctx)
    return ctx.locale == nil or ctx.locale == "en"
end
```

There is no built-in "allowed locales" concept — you express the policy in the
access function, using whatever user fields/roles your project defines.

## CRUD Access in Access Functions

Access functions run with transaction context — they can call `crap.collections.find()` etc. to make decisions based on data in other collections.

> **Note:** Lua CRUD functions enforce access control by default (`overrideAccess = false`). If your access function calls CRUD internally, pass `overrideAccess = true` to avoid recursive access checks:
>
> ```lua
> function M.check(ctx)
>     local count = crap.collections.items.count({ overrideAccess = true })
>     return count < 100  -- allow if under limit
> end
> ```
