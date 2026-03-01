crap.collections.define("posts", {
  labels = {
    singular = "Post",
    plural = "Posts",
  },
  timestamps = true,
  versions = true,
  live = true,
  admin = {
    use_as_title = "title",
    default_sort = "-created_at",
    list_searchable_fields = { "title", "excerpt" },
  },
  fields = {
    {
      name = "title",
      type = "text",
      required = true,
      admin = {
        placeholder = "Enter post title...",
      },
      hooks = {
        before_validate = { "hooks.trim_title" },
      },
    },
    {
      name = "slug",
      type = "text",
      required = true,
      unique = true,
      admin = {
        description = "URL-safe identifier (auto-generated from title)",
        width = "half",
      },
      hooks = {
        before_validate = { "hooks.auto_slug" },
      },
    },
    {
      name = "author",
      type = "relationship",
      required = true,
      relationship = {
        collection = "users",
      },
      admin = {
        width = "half",
      },
    },
    {
      name = "featured_image",
      type = "upload",
      relationship = {
        collection = "media",
      },
      admin = {
        description = "Main image shown in cards and at the top of the post",
        picker = "drawer",
      },
    },
    {
      name = "excerpt",
      type = "textarea",
      required = true,
      admin = {
        description = "Short summary for cards and SEO (max 160 characters)",
        placeholder = "A brief summary of this post...",
      },
    },
    {
      name = "content",
      type = "blocks",
      min_rows = 1,
      max_rows = 20,
      admin = {
        -- Lua function for computed row labels (all block types).
        -- Falls back to per-block label_field if the function returns nil.
        row_label = "labels.content_block_row",
        init_collapsed = true,
        labels = { singular = "Block", plural = "Blocks" },
        picker = "card",
      },
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
          -- label_field: sub-field name used as row label in the admin UI
          label_field = "caption",
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
          type = "code",
          label = "Code",
          label_field = "language",
          fields = {
            {
              name = "language",
              type = "select",
              options = {
                { label = "JavaScript", value = "javascript" },
                { label = "TypeScript", value = "typescript" },
                { label = "Rust", value = "rust" },
                { label = "Python", value = "python" },
                { label = "Lua", value = "lua" },
                { label = "HTML", value = "html" },
                { label = "CSS", value = "css" },
                { label = "Shell", value = "shell" },
              },
            },
            { name = "code", type = "textarea", required = true },
          },
        },
        {
          type = "quote",
          label = "Quote",
          label_field = "attribution",
          fields = {
            { name = "text", type = "textarea", required = true },
            { name = "attribution", type = "text" },
          },
        },
      },
    },
    {
      name = "post_type",
      type = "select",
      required = true,
      default_value = "article",
      options = {
        { label = "Article", value = "article" },
        { label = "Link", value = "link" },
        { label = "Video", value = "video" },
      },
      admin = {
        position = "sidebar",
      },
    },
    -- Collapsible: groups publishing options in a toggleable section
    {
      name = "publishing_options",
      type = "collapsible",
      admin = {
        label = "Publishing Options",
        collapsed = true,
      },
      fields = {
        {
          name = "external_url",
          type = "text",
          admin = {
            label = "External URL",
            placeholder = "https://example.com",
            description = "URL for link/video posts (shown only for non-article types)",
            condition = "hooks.posts.show_media_url",
          },
        },
        {
          name = "hide_from_feed",
          type = "checkbox",
          default_value = false,
          admin = {
            label = "Hide from Feed",
            description = "Exclude this post from RSS feeds and listing pages",
          },
        },
      },
    },
    {
      name = "category",
      type = "relationship",
      relationship = {
        collection = "categories",
      },
      admin = {
        width = "half",
        position = "sidebar",
      },
    },
    {
      name = "tags",
      type = "relationship",
      relationship = {
        collection = "tags",
        has_many = true,
      },
      admin = {
        position = "sidebar",
      },
    },
    {
      name = "published_at",
      type = "date",
      picker_appearance = "dayAndTime",
      admin = {
        description = "Schedule publication (defaults to now when published)",
        width = "half",
        position = "sidebar",
      },
    },
    -- SEO fields are added automatically by plugins/seo.lua
  },
  hooks = {
    before_change = { "hooks.set_published_at" },
  },
  access = {
    read = "access.published_or_author",
    create = "access.authenticated",
    update = "access.author_or_editor",
    delete = "access.author_or_admin",
  },
})
