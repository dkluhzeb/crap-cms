-- Hook lifecycle test fixture — init.lua
-- Register a global hook to verify execution order.
crap.hooks.register("before_change", function(ctx)
    if ctx.data then
        ctx.data._global_hook_ran = "true"
    end
    return ctx
end)
