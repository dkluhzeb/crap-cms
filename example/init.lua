crap.log.info("Crap Studio initializing...")

-- Load plugins (runs after collections/*.lua are loaded)
require("plugins.seo").install({ exclude = { "pages", "inquiries" } })

-- ── Helpers ──────────────────────────────────────────────────

--- Escape a string for safe HTML output.
local function html_escape(s)
	if not s then return "" end
	return tostring(s)
		:gsub("&", "&amp;")
		:gsub("<", "&lt;")
		:gsub(">", "&gt;")
		:gsub('"', "&quot;")
		:gsub("'", "&#39;")
end

-- ── Custom richtext nodes ────────────────────────────────────

-- Block-level: Call to Action button
crap.richtext.register_node("cta", {
	label = "Call to Action",
	inline = false,
	attrs = {
		crap.fields.text({
			name = "text",
			required = true,
			min_length = 2,
			max_length = 80,
			admin = { label = "Button Text", description = "The visible text on the button" },
		}),
		crap.fields.text({
			name = "url",
			required = true,
			admin = { label = "URL", placeholder = "https://..." },
		}),
		crap.fields.select({ name = "style", admin = { label = "Style" }, options = {
			{ label = "Primary", value = "primary" },
			{ label = "Secondary", value = "secondary" },
			{ label = "Outline", value = "outline" },
		}}),
		crap.fields.number({
			name = "padding",
			min = 0,
			max = 100,
			admin = { label = "Padding", step = "1", width = "50%", description = "Vertical padding in pixels" },
		}),
	},
	searchable_attrs = { "text" },
	render = function(attrs)
		local style = ""
		if attrs.padding and attrs.padding ~= "" then
			style = string.format(' style="padding: %spx 0"', html_escape(attrs.padding))
		end
		return string.format(
			'<a href="%s" class="btn btn--%s"%s>%s</a>',
			html_escape(attrs.url), html_escape(attrs.style or "primary"), style, html_escape(attrs.text)
		)
	end,
})

-- Inline: @mention pill
crap.richtext.register_node("mention", {
	label = "Mention",
	inline = true,
	attrs = {
		crap.fields.text({ name = "name", required = true, admin = { label = "Name" } }),
		crap.fields.text({ name = "user_id", admin = { label = "User ID" } }),
	},
	searchable_attrs = { "name" },
	render = function(attrs)
		return string.format('<span class="mention">@%s</span>', html_escape(attrs.name))
	end,
})

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

-- ── Custom admin page: System info ───────────────────────────
--
-- Template lives at `templates/pages/system_info.hbs`. The route
-- `/admin/p/system_info` is created automatically. This call adds the
-- sidebar entry and locks the page to admins via the `admin_only`
-- access function in `access/admin_only.lua`.
--
-- TODO (post-refactor): a `crap-cms make page <slug>` scaffold command
-- will generate the .hbs, the optional `crap.template_data.register`
-- snippet, and this `crap.pages.register` block in one shot. Tracked in
-- the project memory under "scaffold widgets" (post-Phase 3/4).
crap.pages.register("system_info", {
	section = "Tools",
	label = "System info",
	icon = "monitoring",
	access = "access.admin_only",
})

-- Live counters for the System info page. Invoked lazily from the
-- template via `{{#with (data "system_info_counts")}}…{{/with}}`, so
-- pages that don't reference it pay no cost.
---@param ctx crap.template_ctx
---@return table
crap.template_data.register("system_info_counts", function(ctx)
	local nav = ctx.nav or {}
	return {
		collections = nav.collections and #nav.collections or 0,
		globals = nav.globals and #nav.globals or 0,
		custom_pages = nav.custom_pages and #nav.custom_pages or 0,
	}
end)

crap.log.info("Crap Studio ready")
