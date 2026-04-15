--- Access hooks used by the admin globals HTTP access-denied regression tests.
local M = {}

--- Admin-only gate. Used as `read` and `update` access on the restricted global.
--- Returns true iff the current user document has `role == "admin"`.
function M.admin_only(ctx)
    return ctx.user ~= nil and ctx.user.role == "admin"
end

return M
