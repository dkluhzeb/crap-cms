# Filter Constraints

Read access functions can return a filter table instead of a boolean. The filters are merged as AND clauses into the query, restricting which documents the user can see.

## Basic Usage

```lua
function M.own_posts(ctx)
    if ctx.user == nil then return false end
    if ctx.user.role == "admin" then return true end
    -- Regular users can only see their own posts
    return { author = ctx.user.id }
end
```

When this function returns `{ author = ctx.user.id }`, the query gets an additional `WHERE author = ?` clause. The user only sees documents where `author` matches their ID.

## Filter Format

The returned table uses the same format as `crap.collections.find()` filters:

```lua
-- Simple equality
return { category = "news" }

-- Operator-based filter
return { category = { not_equals = "archived" } }

-- Multiple constraints (AND)
return {
    category = "news",
    department = ctx.user.department,
}
```

> **Constrain by your own fields, never by status or lifecycle.** Filter
> constraints may only reference your collection's own fields. The system
> columns `_status` (published vs draft) and `_deleted_at` (live vs trashed) are
> **rejected** — published/draft/trash visibility is the job of the `read`,
> `draft`, and `trash` access keys, which scope each view automatically. Don't
> return `{ _status = "published" }` to hide drafts; a plain `read` rule already
> covers published content only.

## How Constraints Are Merged

Constraints from access functions are merged with any existing query filters using AND:

```
Final WHERE = (user's filters) AND (access constraints)
```

This means constraints can only **narrow** results, never expand them.

> **Where constraints are evaluated.** On a direct `Find`/`count`, constraints
> compile to SQL `WHERE` clauses. When the same collection is reached as a
> *populated* relationship or join target, its constraints are instead matched
> **in memory** against the embedded row. The two agree exactly for plain
> equality/membership on your own fields (`{ author = ctx.user.id }`,
> `{ tenant_id = ... }`); exotic operators like `like` pattern matching or
> cross-type numeric comparisons can differ at the edges. Keep access
> constraints to simple field equality — the recommended shape anyway.

## Example: Multi-Tenant Access

```lua
function M.tenant_read(ctx)
    if ctx.user == nil then return false end
    -- Users can only see documents in their tenant
    return { tenant_id = ctx.user.tenant_id }
end
```

## Example: Owner-or-Admin

Published vs draft visibility is **not** something a constraint should express —
that is what the `read` and `draft` keys are for (a plain `read` rule already
scopes published content; grant `draft` to expose unpublished content). A
constraint narrows *which rows within a view* the caller may see, by your own
fields:

```lua
function M.own_posts(ctx)
    if ctx.user == nil then return false end  -- anonymous: no access
    if ctx.user.role == "admin" then
        return true  -- admins see everything
    end
    -- Everyone else sees only their own rows.
    -- (Note: complex OR logic isn't supported in filter returns.)
    return { author = ctx.user.id }
end
```

To expose published content to anonymous readers, return `true` (or a
non-ownership constraint) from `read` — the read view is already published-only.
