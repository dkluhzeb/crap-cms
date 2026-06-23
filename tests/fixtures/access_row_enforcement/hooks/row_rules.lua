--- Access control functions that return a filter table for row-level enforcement.
local M = {}

--- Require any authenticated user (create has no target row; filter-table returns are rejected).
function M.authenticated(ctx)
    return ctx.user ~= nil
end

--- Non-admins can only operate on rows where author_id equals their user id.
--- Admins get unrestricted access.
function M.own_rows(ctx)
    if ctx.user == nil then return false end
    if ctx.user.role == "admin" then return true end
    return { author_id = ctx.user.id }
end

--- Create access that mistakenly returns a filter table — used to exercise
--- the "Constrained on create is rejected" path.
function M.create_returns_filter(ctx)
    return { author_id = "whoever" }
end

--- Field-level write access: only admins may write the field. Used to prove a
--- version restore cannot overwrite a write-locked field for a non-admin who
--- otherwise has collection `update` access.
function M.admin_only_field(ctx)
    return ctx.user ~= nil and ctx.user.role == "admin"
end

return M
