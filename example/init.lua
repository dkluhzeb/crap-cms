crap.log.info("Crap Studio initializing...")

-- Load plugins (runs after collections/*.lua are loaded)
require("plugins.seo").install({ exclude = { "pages", "inquiries" } })

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
			style = string.format(' style="padding: %spx 0"', attrs.padding)
		end
		return string.format(
			'<a href="%s" class="btn btn--%s"%s>%s</a>',
			attrs.url, attrs.style or "primary", style, attrs.text
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
		return string.format('<span class="mention">@%s</span>', attrs.name)
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

crap.log.info("Crap Studio ready")
