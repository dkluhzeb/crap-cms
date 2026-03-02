crap.log.info("Meridian Studio initializing...")

-- Load plugins (runs after collections/*.lua are loaded)
require("plugins.seo").install({ exclude = { "pages", "inquiries" } })

-- Global hook: log all content changes
---@param context crap.HookContext
---@return crap.HookContext
crap.hooks.register("after_change", function(context)
	local op = context.operation or "unknown"
	local collection = context.collection or "unknown"
	local id = context.data and context.data.id or "?"
	crap.log.info(string.format("[audit] %s/%s %s", collection, id, op))
	return context
end)

crap.log.info("Meridian Studio ready")
