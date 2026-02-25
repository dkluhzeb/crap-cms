--- After-write hooks that exercise CRUD access inside transactions.
local M = {}

--- after_change hook that creates a side-effect document in the same collection.
--- Used to verify that after_change hooks have CRUD access inside the transaction.
function M.create_side_effect(ctx)
    crap.collections.create("articles", {
        title = "side-effect-from-after-hook",
        body = "created by after_change hook",
        status = "published",
    })
    return ctx
end

--- after_change hook that intentionally errors to test rollback.
function M.error_hook(ctx)
    error("intentional after_change error for rollback test")
end

--- after_change hook that reads ctx.context to verify it flows from before-hooks.
function M.check_context(ctx)
    if ctx.context and ctx.context.before_marker then
        ctx.data._context_received = ctx.context.before_marker
    end
    return ctx
end

return M
