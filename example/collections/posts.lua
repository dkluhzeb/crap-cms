crap.collections.define("posts", {
  labels = { singular = "Post", plural = "Posts" },
  timestamps = true,
  versions = true,
  live = true,
  soft_delete = true,
  soft_delete_retention = "30d",
  admin = {
    use_as_title = "title",
    default_sort = "-published_at",
    list_searchable_fields = { "title", "slug", "excerpt" },
  },
  fields = {
    crap.fields.text({
      name = "title",
      required = true,
      hooks = { before_validate = { "hooks.trim_title" } },
      admin = { placeholder = "Post title" },
    }),
    crap.fields.text({
      name = "slug",
      required = true,
      unique = true,
      hooks = { before_validate = { "hooks.auto_slug" } },
    }),
    crap.fields.textarea({
      name = "excerpt",
      admin = { rows = 3, placeholder = "Brief summary for listings and SEO" },
    }),
    -- Sidebar fields
    crap.fields.select({
      name = "post_type",
      required = true,
      default_value = "article",
      options = {
        { label = "Article", value = "article" },
        { label = "Case Study", value = "case_study" },
        { label = "Link", value = "link" },
        { label = "Video", value = "video" },
      },
      admin = { position = "sidebar" },
    }),
    crap.fields.date({
      name = "published_at",
      picker_appearance = "dayAndTime",
      admin = { position = "sidebar" },
    }),
    crap.fields.text({
      name = "external_url",
      admin = {
        placeholder = "https://...",
        condition = {
          field = "post_type",
          condition = "one_of",
          value = { "link", "video" },
        },
      },
    }),
    -- Relationships
    crap.fields.relationship({
      name = "author",
      required = true,
      relationship = { collection = "users" },
    }),
    crap.fields.upload({ name = "hero_image", relationship = { collection = "media" } }),
    crap.fields.relationship({
      name = "categories",
      relationship = { collection = "categories", has_many = true },
    }),
    crap.fields.relationship({
      name = "tags",
      relationship = { collection = "tags", has_many = true },
    }),
    -- Content
    crap.fields.richtext({
      name = "content",
      admin = {
        format = "json",
        nodes = { "cta", "mention" },
        features = {
          "bold",
          "italic",
          "link",
          "heading",
          "blockquote",
          "bulletList",
          "orderedList",
          "code",
          "codeBlock",
          "horizontalRule",
        },
      },
    }),
    -- Reading time (virtual, computed by after_read hook)
    crap.fields.text({
      name = "reading_time",
      admin = { readonly = true, position = "sidebar" },
      hooks = { after_read = { "hooks.reading_time" } },
    }),
    -- Polymorphic relationship: related posts OR projects
    crap.fields.relationship({
      name = "related_content",
      relationship = {
        collection = { "posts", "projects" },
        has_many = true,
        max_depth = 1,
      },
      admin = { description = "Related posts or projects" },
    }),
    -- Publishing collapsible
    crap.fields.collapsible({
      name = "publishing",
      admin = { label = "Publishing" },
      fields = {
        crap.fields.checkbox({ name = "featured", default_value = false }),
        crap.fields.checkbox({ name = "pinned", default_value = false }),
      },
    }),
  },
  hooks = {
    before_change = { "hooks.set_published_at" },
  },
  access = {
    read = "access.published_or_author",
    create = "access.authenticated",
    update = "access.author_or_editor",
    trash = "access.editor_or_above",
    delete = "access.admin_or_director",
  },
})
