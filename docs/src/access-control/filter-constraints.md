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
return { status = "published" }

-- Operator-based filter
return { status = { not_equals = "archived" } }

-- Multiple constraints (AND)
return {
    status = "published",
    department = ctx.user.department,
}
```

## How Constraints Are Merged

Constraints from access functions are merged with any existing query filters using AND:

```
Final WHERE = (user's filters) AND (access constraints)
```

This means constraints can only **narrow** results, never expand them.

## Example: Multi-Tenant Access

```lua
function M.tenant_read(ctx)
    if ctx.user == nil then return false end
    -- Users can only see documents in their tenant
    return { tenant_id = ctx.user.tenant_id }
end
```

## Example: Published-Only for Anonymous

```lua
function M.public_or_own(ctx)
    if ctx.user == nil then
        return { status = "published" }
    end
    if ctx.user.role == "admin" then
        return true  -- admins see everything
    end
    -- Authors see their own + published
    -- (Note: complex OR logic isn't supported in filter returns)
    return { author = ctx.user.id }
end
```
