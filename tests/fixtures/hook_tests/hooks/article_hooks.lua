--- Collection-level hooks for articles.
local M = {}

--- before_validate: ensure title is trimmed.
function M.before_validate(ctx)
    if ctx.data and ctx.data.title then
        ctx.data.title = ctx.data.title:match("^%s*(.-)%s*$")
    end
    return ctx
end

--- before_change: set default status if not provided.
function M.before_change(ctx)
    if ctx.data and (ctx.data.status == nil or ctx.data.status == "") then
        ctx.data.status = "draft"
    end
    -- Add a marker so we can verify this hook ran
    ctx.data._hook_ran = "before_change"
    return ctx
end

--- after_change: fire-and-forget hook (no CRUD access).
function M.after_change(ctx)
    crap.log.info("after_change fired for " .. (ctx.collection or "unknown"))
end

--- after_read: add a marker to verify the hook ran.
function M.after_read(ctx)
    if ctx.data then
        ctx.data._was_read = "true"
    end
    return ctx
end

return M
