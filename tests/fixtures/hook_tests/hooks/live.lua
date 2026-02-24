--- Live event filter functions.
local M = {}

--- Filter: allow create and update, suppress delete.
function M.filter_published(ctx)
    return ctx.operation ~= "delete"
end

--- before_broadcast hook that transforms data.
function M.transform_broadcast(ctx)
    if ctx.data then
        ctx.data._broadcast_marker = "transformed"
    end
    return ctx
end

--- before_broadcast hook that suppresses the event.
function M.suppress_broadcast(ctx)
    return nil
end

return M
