-- init.lua -- runs once at startup.
-- Register global hooks, load plugins, or set up shared state.

-- Example: register a global hook that runs for ALL collections
-- crap.hooks.register("after_change", function(context)
--     crap.log.info("Document changed: " .. context.collection .. "/" .. (context.data.id or ""))
-- end)

-- Example: load a plugin module
-- require("plugins.seo")

-- Example: register a custom HTTP route (scaffold one with `crap-cms make route`)
-- crap.routes.register({
--     path = "/health",
--     method = "GET",
--     handler = "routes.health",
-- })

-- Example: expose data to admin templates via the {{data "name"}} helper
-- crap.template_data.register("stats", function()
--     return { total = crap.collections.posts.count() }
-- end)

crap.log.info("init.lua loaded")
