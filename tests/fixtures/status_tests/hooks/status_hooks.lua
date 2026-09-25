--- Hooks for the status-reporting tests.
local M = {}

--- after_change: remember the `_status` the hook was handed.
function M.record_status(ctx)
    _G._after_change_status = ctx.data._status
    return ctx
end

return M
