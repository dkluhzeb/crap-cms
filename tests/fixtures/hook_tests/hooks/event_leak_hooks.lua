--- Hooks for the event-strip-ordering regression test.
local M = {}

--- Field access: always deny reads of `secret`.
function M.deny(ctx)
    return false
end

--- after_read: copy whatever it can see of `secret` into `summary`.
function M.copy_secret(ctx)
    if ctx.data then
        ctx.data.summary = "seen:" .. tostring(ctx.data.secret)
    end
    return ctx
end

return M
