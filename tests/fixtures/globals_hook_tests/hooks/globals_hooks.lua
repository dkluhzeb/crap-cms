--- Hooks used by the globals hook lifecycle tests.
local M = {}

--- before_validate: normalise + add a marker to prove the hook ran.
function M.before_validate(ctx)
    if ctx.data and ctx.data.title then
        ctx.data.title = ctx.data.title:match("^%s*(.-)%s*$")
    end
    ctx.data._bv_marker = "ran"
    return ctx
end

--- before_change: abort the update when site_name is the poisoned sentinel.
function M.before_change_abort_on_poison(ctx)
    if ctx.data and ctx.data.site_name == "POISON" then
        error("aborted by before_change hook")
    end
    return ctx
end

--- after_read: uppercase the `tagline` field so tests can observe the transform.
function M.uppercase_tagline(ctx)
    if ctx.data and type(ctx.data.tagline) == "string" then
        ctx.data.tagline = ctx.data.tagline:upper()
    end
    return ctx
end

--- Admin-only access gate used by globals access tests.
function M.admin_only(ctx)
    return ctx.user ~= nil and ctx.user.role == "admin"
end

return M
