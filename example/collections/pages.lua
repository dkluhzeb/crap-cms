crap.collections.define("pages", {
	labels = {
		singular = "Page",
		plural = "Pages",
	},
	timestamps = true,
	versions = true,
	admin = {
		use_as_title = "title",
		default_sort = "title",
		list_searchable_fields = { "title", "slug" },
	},
	fields = {
		{
			name = "title",
			type = "text",
			required = true,
			localized = true,
		},
		{
			name = "slug",
			type = "text",
			required = true,
			unique = true,
			localized = true,
			admin = {
				description = "URL path (e.g., 'about' for /about)",
				width = "half",
			},
			hooks = {
				before_validate = { "hooks.auto_slug" },
			},
		},
		{
			name = "content",
			type = "blocks",
			localized = true,
			blocks = {
				{
					type = "richtext",
					label = "Rich Text",
					fields = {
						{ name = "body", type = "richtext" },
					},
				},
				{
					type = "image",
					label = "Image",
					fields = {
						{
							name = "image",
							type = "upload",
							required = true,
							relationship = { collection = "media" },
						},
						{ name = "caption", type = "text" },
					},
				},
				{
					type = "cta",
					label = "Call to Action",
					fields = {
						{ name = "heading", type = "text", required = true },
						{ name = "body", type = "textarea" },
						{ name = "button_text", type = "text", required = true },
						{ name = "button_url", type = "text", required = true },
					},
				},
				{
					name = "deep",
					type = "blocks",
					localized = true,
					blocks = {
						{
							type = "richtext",
							label = "Rich Text",
							fields = {
								{ name = "body", type = "richtext" },
							},
						},
						{
							type = "image",
							label = "Image",
							fields = {
								{
									name = "image",
									type = "upload",
									required = true,
									relationship = { collection = "media" },
								},
								{ name = "caption", type = "text" },
							},
						},
						{
							type = "cta",
							label = "Call to Action",
							fields = {
								{ name = "heading", type = "text", required = true },
								{ name = "body", type = "textarea" },
								{ name = "button_text", type = "text", required = true },
								{ name = "button_url", type = "text", required = true },
							},
						},
					},
				},
			},
		},
		-- SEO group
		{
			name = "seo",
			type = "group",
			admin = {
				label = "SEO",
				collapsed = true,
				position = "sidebar",
			},
			fields = {
				{
					name = "meta_title",
					type = "text",
					localized = true,
					admin = {
						label = "Meta Title",
						placeholder = "Custom SEO title...",
					},
				},
				{
					name = "meta_description",
					type = "textarea",
					localized = true,
					admin = {
						label = "Meta Description",
						placeholder = "Describe this page for search engines...",
					},
				},
				{
					name = "og_image",
					type = "upload",
					relationship = { collection = "media" },
					admin = {
						label = "Social Image",
					},
				},
				{
					name = "no_index",
					type = "checkbox",
					default_value = false,
					admin = {
						label = "No Index",
					},
				},
			},
		},
	},
	access = {
		read = "access.anyone",
		create = "access.editor_or_admin",
		update = "access.editor_or_admin",
		delete = "access.admin_only",
	},
})
