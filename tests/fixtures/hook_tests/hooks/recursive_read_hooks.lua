--- Hooks for the read-recursion depth-cap regression test.
local M = {}

--- before_read: read the same collection again (unbounded without a cap).
function M.read_again(ctx)
    _G._read_depth_counter = (_G._read_depth_counter or 0) + 1
    crap.collections.find("recursive_read", {})
    return ctx
end

return M
