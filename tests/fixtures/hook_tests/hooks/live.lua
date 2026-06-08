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

--- Filter that gates on the document id and the editing user — verifies that
--- `ctx.document_id` and `ctx.edited_by` reach the live filter.
function M.gate_on_id_and_editor(ctx)
    if ctx.document_id ~= "broadcast-me" then return false end
    if ctx.edited_by == nil then return false end
    return ctx.edited_by.email == "editor@x.com"
end

return M
