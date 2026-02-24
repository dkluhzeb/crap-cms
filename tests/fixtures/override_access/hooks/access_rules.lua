--- Access control functions for overrideAccess tests.
local M = {}

--- Allow everyone (including anonymous).
function M.public_read(ctx)
    return true
end

--- Require any authenticated user.
function M.authenticated(ctx)
    return ctx.user ~= nil
end

--- Require admin role.
function M.admin_only(ctx)
    return ctx.user ~= nil and ctx.user.role == "admin"
end

--- Admins see everything; others see only their own (by owner field).
function M.own_or_admin(ctx)
    if ctx.user == nil then return false end
    if ctx.user.role == "admin" then return true end
    return { owner = ctx.user.id }
end

return M
