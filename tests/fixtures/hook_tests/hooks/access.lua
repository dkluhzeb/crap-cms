--- Access control test functions.
local M = {}

--- Always allow access.
function M.allow_all(ctx)
    return true
end

--- Always deny access.
function M.deny_all(ctx)
    return false
end

--- Return a constraint table (read filter).
function M.constrained(ctx)
    return { status = "published" }
end

--- Check if user has admin role.
function M.check_role(ctx)
    if ctx.user and ctx.user.role == "admin" then
        return true
    end
    return false
end

--- Field-level read deny (always denies).
function M.field_read_deny(ctx)
    return false
end

--- Field-level write deny (always denies).
function M.field_write_deny(ctx)
    return false
end

return M
