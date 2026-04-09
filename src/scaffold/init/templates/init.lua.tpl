-- init.lua -- runs once at startup.
-- Register global hooks, load plugins, or set up shared state.

-- Example: register a global hook that runs for ALL collections
-- crap.hooks.register("after_change", function(context)
--     crap.log.info("Document changed: " .. context.collection .. "/" .. (context.data.id or ""))
-- end)

-- Example: load a plugin module
-- require("plugins.seo")

crap.log.info("init.lua loaded")
