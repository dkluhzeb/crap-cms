-- init.lua — Six Seven blog
-- Runs once at startup, after collections/*.lua are loaded.
-- Install plugins, register global hooks, log startup info.

crap.log.info("Six Seven blog initializing...")

-- Plugins (run after collection definitions are loaded)
require("plugins.seo").install()

-- Global hook: log all content changes
crap.hooks.register("after_change", function(context)
    local op = context.operation or "unknown"
    local collection = context.collection or "unknown"
    local id = context.data and context.data.id or "?"
    crap.log.info(string.format("[%s] %s %s", collection, op, id))
end)

crap.log.info("Six Seven blog ready")
