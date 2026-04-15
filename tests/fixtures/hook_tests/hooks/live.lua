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

--- before_broadcast hook that overrides the title in the event payload.
--- Used to verify that data mutations in before_broadcast affect the event
--- payload but NOT the stored document.
function M.mutate_title_for_broadcast(ctx)
    if ctx.data then
        ctx.data.title = "mutated"
    end
    return ctx
end

return M
