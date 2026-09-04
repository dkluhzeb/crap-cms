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

-- Membership in a set
return { dept = { ["in"] = { "eng", "design" } } }

-- Multiple constraints (AND)
return {
    category = "news",
    department = ctx.user.department,
}

-- OR groups: the caller sees a row when ANY group matches
return {
    ["or"] = {
        { author = ctx.user.id },
        { visibility = "public" },
    },
}
```

The table is decoded by the **same canonical `where` grammar** as every CRUD
filter on every surface — scalar shorthand, operator tables, and `["or"]`
groups all behave exactly as they do in `crap.collections.find()`. The
restrictions below apply per *leaf* filter, inside `or` groups included.

## Allowed Operators — Equality and Membership Only

Access constraints may only use **equality** and **membership** operators:

| Operator | Meaning |
| --- | --- |
| `equals` (bare value) | field equals value |
| `not_equals` | field differs from value |
| `in` | field is one of a set |
| `not_in` | field is none of a set |
| `exists` | field is present (non-null) |
| `not_exists` | field is absent (null) |

Pattern operators (`like`, `contains`) and ordered operators (`greater_than`,
`less_than`, `greater_than_or_equal`, `less_than_or_equal`) are **rejected** in
an access rule — returning one is a configuration error that fails the request
with a clear message naming the operator.

**Why the restriction?** Every access constraint is enforced on two paths: it
compiles to a SQL `WHERE` clause for direct reads, *and* it is evaluated
in-memory for surfaces that can't hit the database (live event streams,
relationship/join population — see *Where constraints are evaluated* below).
Pattern and ordered matching can diverge between those paths (collation, `LIKE`
semantics, numeric vs text affinity), and the safe default on divergence is to
*show* the row — i.e. a leak. Restricting access rules to equality and
membership makes that divergence impossible by construction.

You lose nothing real. Access control is fundamentally about **identity,
ownership, and membership** — "is this row *mine* / my *tenant's* / in my
*set*?" — which equality and membership express exactly. Value-querying (ranges,
prefixes, substrings) is a property of a *user query*, where the full operator
set is still available; it just doesn't belong in the rule that decides *who may
see what*.

### Re-modeling the value-based patterns

If you reach for `like` or an ordered comparison in an access rule, there is
almost always a cleaner, exact model:

```lua
-- ❌ Time-window via ordered comparison — diverges, and recomputes `now()`
function M.recent_only(ctx)
    local cutoff = os.date("!%Y-%m-%dT%H:%M:%SZ", os.time() - 30 * 24 * 3600)
    return { created_at = { greater_than = cutoff } }  -- rejected
end
-- ✅ A time window is a *system* concern. Gate it with a published/draft
--    view or a dedicated capability rather than an ad-hoc range in the rule.

-- ❌ Clearance via ordered comparison
function M.clearance(ctx)
    return { level = { less_than_or_equal = ctx.user.clearance } }  -- rejected
end
-- ✅ Clearance levels are a small discrete set — that is membership:
function M.clearance(ctx)
    return { level = { ["in"] = levels_up_to(ctx.user.clearance) } }  -- {1,2,3}
end

-- ❌ Tenant-by-domain via suffix match (also a fragile string parse)
function M.same_domain(ctx)
    local domain = ctx.user.email:match("@(.+)$")
    return { email = { like = "%@" .. domain } }  -- rejected
end
-- ✅ Store the domain as a field and match it exactly:
function M.same_domain(ctx)
    return { tenant_domain = ctx.user.tenant_domain }
end

-- ❌ Department-tree via path prefix
function M.dept_tree(ctx)
    return { path = { like = ctx.user.dept_prefix .. "%" } }  -- rejected
end
-- ✅ Model the hierarchy as an ancestor/department field and use membership:
function M.dept_tree(ctx)
    return { dept = { ["in"] = ctx.user.dept_and_children } }
end
```

> **Constrain by your own fields, never by status or lifecycle.** Filter
> constraints may only reference your collection's own fields. The system
> columns `_status` (published vs draft) and `_deleted_at` (live vs trashed) are
> **rejected** — published/draft/trash visibility is the job of the `read`,
> `draft`, and `trash` access keys, which scope each view automatically. Don't
> return `{ _status = "published" }` to hide drafts; a plain `read` rule already
> covers published content only.
>
> One exception: a `_status` constraint from the **`update`** rule is accepted
> for **bulk updates** on a drafts-enabled collection — `update_many` itself
> targets published rows there (unless `draft = true`), so the operation
> already owns the status dimension the constraint refines.

## How Constraints Are Merged

Constraints from access functions are merged with any existing query filters using AND:

```
Final WHERE = (user's filters) AND (access constraints)
```

This means constraints can only **narrow** results, never expand them.

> **Where constraints are evaluated.** On a direct `Find`/`count`, constraints
> compile to SQL `WHERE` clauses. When the same collection is reached as a
> *populated* relationship or join target — or gated on a live event stream —
> its constraints are instead matched **in memory** against the row. Because the
> [allowed operators](#allowed-operators--equality-and-membership-only) are
> limited to equality and membership, the two paths agree exactly — by
> construction. That is the whole reason pattern and ordered operators are
> rejected: they are the operators whose SQL and in-memory results could
> diverge.

## Example: Multi-Tenant Access

```lua
function M.tenant_read(ctx)
    if ctx.user == nil then return false end
    -- Guard the constraint VALUE, not just ctx.user — see the warning below.
    if ctx.user.tenant_id == nil then return false end
    -- Users can only see documents in their tenant
    return { tenant_id = ctx.user.tenant_id }
end
```

> ⚠️ **Always guard the constraint value against `nil`.** In Lua,
> `{ tenant_id = ctx.user.tenant_id }` is an **empty table** `{}` when
> `ctx.user.tenant_id` is `nil` (the table constructor drops nil-valued keys).
> An empty constraint table is **denied** (fail-closed) — it is never treated as
> "allow all" (that is what `return true` is for). So a tenantless user is
> safely refused rather than shown every tenant's data. Guarding the value
> explicitly (as above) makes the intent clear and avoids relying on the
> fail-closed default. The same applies to any `{ field = ctx.user.<field> }`
> where the user field is optional — **including inside `["or"]` groups**: a
> group whose only key is nil-valued becomes an empty group, which would match
> every row, so a constraint containing an empty group is denied the same way.

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

## Field Restrictions

A constraint must reference a **non-localized, top-level own field**. Two field
kinds evaluate inconsistently across the SQL and in-memory enforcement paths and
so are unsupported in access constraints:

- **Localized fields are rejected.** A localized field is stored per-locale
  (`title__en`, `title__de`), so the SQL path resolves the constraint to one
  locale's column while the in-memory path (events, populated relationships)
  matches a flat/active-locale value — they can disagree. Returning a constraint
  on a localized field is a hard error. (This is rarely what you want anyway:
  access is identity/ownership/membership, and `{ title = X }` is ambiguous
  across locales.) Constrain by a non-localized identity field instead.

- **Dotted relationship/JSON paths (`{ ["author.id"] = … }`) are rejected.**
  The SQL path resolves these via subqueries, but the in-memory path matches the
  flat field only and so fails closed (hides rows it should show) for populated
  and live-event surfaces. Returning one is a hard error — denormalize to a flat
  own column (store and constrain `author_id` directly) instead.
