crap.collections.define("pages", {
	labels = {
		singular = "Page",
		plural = "Pages",
	},
	timestamps = true,
	admin = {
		use_as_title = "title",
		default_sort = "title",
	},
	fields = {
		{
			name = "title",
			type = "text",
			required = true,
		},
		{
			name = "slug",
			type = "text",
			required = true,
			unique = true,
		},
		{
			name = "published",
			type = "checkbox",
			default_value = false,
		},
		-- Group field: sub-fields become seo__title, seo__description columns
		{
			name = "seo",
			type = "group",
			fields = {
				{ name = "title", type = "text", admin = { placeholder = "SEO title" } },
				{ name = "description", type = "textarea", admin = { placeholder = "Meta description" } },
				{ name = "no_index", type = "checkbox" },
			},
			admin = {
				description = "Search engine optimization settings",
			},
		},
		-- Upload field: references the media collection
		{
			name = "hero_image",
			type = "upload",
			relation_to = "media",
			admin = {
				description = "Hero image for the page",
			},
		},
		-- Blocks field: flexible content with different block types
		{
			name = "content",
			type = "blocks",
			blocks = {
				{
					type = "richtext",
					label = "Rich Text",
					fields = {
						{ name = "body", type = "richtext" },
					},
				},
				{
					type = "hero",
					label = "Hero Section",
					fields = {
						{ name = "heading", type = "text", required = true },
						{ name = "subheading", type = "text" },
					},
				},
				{
					type = "cta",
					label = "Call to Action",
					fields = {
						{ name = "text", type = "text", required = true },
						{ name = "url", type = "text", required = true },
						{
							name = "style",
							type = "select",
							options = {
								{ label = "Primary", value = "primary" },
								{ label = "Secondary", value = "secondary" },
							},
						},
					},
				},
			},
			admin = {
				description = "Page content sections",
			},
		},
	},
})
