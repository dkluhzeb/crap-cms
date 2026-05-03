crap.collections.define("projects", {
  labels = { singular = "Project", plural = "Projects" },
  timestamps = true,
  versions = true,
  live = true,
  admin = {
    use_as_title = "title",
    default_sort = "-created_at",
    list_searchable_fields = { "title", "slug", "excerpt" },
  },
  fields = {
    crap.fields.text({
      name = "title",
      required = true,
      localized = true,
      hooks = { before_validate = { "hooks.trim_title" } },
      admin = { placeholder = "Project title" },
    }),
    crap.fields.text({
      name = "slug",
      required = true,
      unique = true,
      hooks = { before_validate = { "hooks.auto_slug" } },
      admin = { placeholder = "auto-generated-from-title" },
    }),
    crap.fields.textarea({
      name = "excerpt",
      admin = { rows = 3, placeholder = "Brief project description" },
    }),
    -- Sidebar fields
    crap.fields.select({
      name = "status",
      required = true,
      default_value = "planning",
      options = {
        { label = "Planning", value = "planning" },
        { label = "In Progress", value = "in_progress" },
        { label = "Review", value = "review" },
        { label = "Completed", value = "completed" },
        { label = "Archived", value = "archived" },
      },
      admin = { position = "sidebar" },
    }),
    crap.fields.radio({
      name = "priority",
      default_value = "normal",
      options = {
        { label = "Low", value = "low" },
        { label = "Normal", value = "normal" },
        { label = "High", value = "high" },
        { label = "Urgent", value = "urgent" },
      },
      admin = { position = "sidebar" },
    }),
    crap.fields.checkbox({ name = "featured", default_value = false, admin = { position = "sidebar" } }),
    -- Dates row
    crap.fields.row({
      name = "dates_row",
      fields = {
        crap.fields.date({
          name = "start_date",
          picker_appearance = "dayOnly",
          admin = { width = "half" },
        }),
        crap.fields.date({
          name = "end_date",
          picker_appearance = "dayOnly",
          admin = {
            width = "half",
            condition = { field = "status", condition = "not_equals", value = "planning" },
          },
        }),
      },
    }),
    -- Relationships
    crap.fields.upload({ name = "hero_image", relationship = { collection = "media" } }),
    crap.fields.relationship({ name = "client", relationship = { collection = "clients" } }),
    crap.fields.relationship({ name = "team", relationship = { collection = "users", has_many = true } }),
    crap.fields.relationship({ name = "categories", relationship = { collection = "categories", has_many = true } }),
    crap.fields.relationship({ name = "tags", relationship = { collection = "tags", has_many = true } }),
    -- Budget (field-level access)
    crap.fields.number({
      name = "budget",
      min = 0,
      hooks = { before_validate = { "hooks.validate_budget" } },
      access = {
        read = "access.field_admin_or_director",
        create = "access.field_admin_or_director",
        update = "access.field_admin_or_director",
      },
      admin = { description = "Project budget (visible to admin/director only)" },
    }),
    -- Deliverables array
    crap.fields.array({
      name = "deliverables",
      admin = { label_field = "title", labels = { singular = "Deliverable", plural = "Deliverables" } },
      fields = {
        crap.fields.text({ name = "title", required = true }),
        crap.fields.checkbox({ name = "completed", default_value = false }),
      },
    }),
    -- Content blocks
    crap.fields.blocks({
      name = "content",
      admin = { picker = "card", row_label = "hooks.labels" },
      blocks = {
        {
          type = "richtext",
          label = "Rich Text",
          group = "Content",
          fields = {
            crap.fields.richtext({
              name = "body",
              admin = {
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
                },
              },
            }),
          },
        },
        {
          type = "image_gallery",
          label = "Image Gallery",
          label_field = "caption",
          group = "Media",
          fields = {
            crap.fields.text({ name = "caption" }),
            crap.fields.upload({ name = "images", relationship = { collection = "media", has_many = true } }),
            crap.fields.select({
              name = "columns",
              default_value = "3",
              options = {
                { label = "2 Columns", value = "2" },
                { label = "3 Columns", value = "3" },
                { label = "4 Columns", value = "4" },
              },
            }),
          },
        },
        {
          type = "video_embed",
          label = "Video Embed",
          group = "Media",
          fields = {
            crap.fields.text({
              name = "url",
              required = true,
              admin = { placeholder = "https://youtube.com/watch?v=..." },
            }),
            crap.fields.text({ name = "caption" }),
          },
        },
        {
          type = "stats",
          label = "Stats Row",
          group = "Content",
          fields = {
            crap.fields.array({
              name = "items",
              min_rows = 1,
              max_rows = 4,
              fields = {
                crap.fields.text({ name = "value", required = true, admin = { placeholder = "98%" } }),
                crap.fields.text({ name = "label", required = true, admin = { placeholder = "Client satisfaction" } }),
              },
            }),
          },
        },
        {
          type = "testimonial",
          label = "Testimonial",
          group = "Content",
          fields = {
            crap.fields.textarea({ name = "quote", required = true }),
            crap.fields.text({ name = "author_name", required = true }),
            crap.fields.text({ name = "author_title" }),
          },
        },
        {
          type = "code_block",
          label = "Code",
          group = "Technical",
          fields = {
            crap.fields.code({
              name = "code",
              admin = {
                language = "javascript",
                languages = { "javascript", "json", "html", "css", "python" },
              },
            }),
            crap.fields.text({ name = "caption" }),
          },
        },
      },
    }),
    -- Publishing options (collapsible)
    crap.fields.collapsible({
      name = "publishing_options",
      admin = { label = "Publishing Options" },
      fields = {
        crap.fields.date({ name = "published_at", picker_appearance = "dayAndTime" }),
        crap.fields.text({ name = "external_url", admin = { placeholder = "https://..." } }),
      },
    }),
  },
  hooks = {
    before_change = { "hooks.set_published_at" },
  },
  access = {
    read = "access.anyone",
    create = "access.editor_or_above",
    update = "access.team_or_admin",
    delete = "access.admin_or_director",
  },
})
