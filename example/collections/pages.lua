crap.collections.define("pages", {
  labels = { singular = "Page", plural = "Pages" },
  timestamps = true,
  versions = true,
  soft_delete = true,
  soft_delete_retention = "90d",
  admin = {
    use_as_title = "title",
    default_sort = "title",
    list_searchable_fields = { "title", "slug" },
  },
  fields = {
    crap.fields.text({
      name = "title",
      required = true,
      localized = true,
      hooks = { before_validate = { "hooks.trim_title" } },
      admin = { placeholder = "Page title" },
    }),
    crap.fields.text({
      name = "slug",
      required = true,
      unique = true,
      localized = true,
      hooks = { before_validate = { "hooks.auto_slug" } },
    }),
    -- Page settings (tabs layout)
    crap.fields.tabs({
      name = "page_settings",
      tabs = {
        {
          label = "Content",
          fields = {
            crap.fields.blocks({
              name = "content",
              localized = true,
              blocks = {
                {
                  type = "hero",
                  label = "Hero Section",
                  group = "Layout",
                  fields = {
                    crap.fields.text({ name = "heading", required = true }),
                    crap.fields.text({ name = "subheading" }),
                    crap.fields.upload({ name = "background", relationship = { collection = "media" } }),
                    crap.fields.text({ name = "cta_text", admin = { placeholder = "Get in touch" } }),
                    crap.fields.text({ name = "cta_url", admin = { placeholder = "/contact" } }),
                  },
                },
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
                        },
                      },
                    }),
                  },
                },
                {
                  type = "two_column",
                  label = "Two Columns",
                  group = "Layout",
                  fields = {
                    crap.fields.richtext({ name = "left" }),
                    crap.fields.richtext({ name = "right" }),
                  },
                },
                {
                  type = "image_text",
                  label = "Image + Text",
                  group = "Layout",
                  fields = {
                    crap.fields.upload({ name = "image", relationship = { collection = "media" } }),
                    crap.fields.richtext({ name = "body" }),
                    crap.fields.select({
                      name = "image_position",
                      default_value = "left",
                      options = {
                        { label = "Left", value = "left" },
                        { label = "Right", value = "right" },
                      },
                    }),
                  },
                },
                {
                  type = "cta_banner",
                  label = "CTA Banner",
                  group = "Content",
                  fields = {
                    crap.fields.text({ name = "heading", required = true }),
                    crap.fields.textarea({ name = "description" }),
                    crap.fields.text({ name = "button_text" }),
                    crap.fields.text({ name = "button_url" }),
                  },
                },
                {
                  type = "team_grid",
                  label = "Team Grid",
                  group = "Content",
                  fields = {
                    crap.fields.text({ name = "heading" }),
                    crap.fields.relationship({
                      name = "members",
                      relationship = { collection = "users", has_many = true },
                    }),
                  },
                },
                {
                  type = "services_list",
                  label = "Services List",
                  group = "Content",
                  fields = {
                    crap.fields.text({ name = "heading" }),
                    crap.fields.relationship({
                      name = "services",
                      relationship = { collection = "services", has_many = true },
                    }),
                  },
                },
              },
            }),
          },
        },
        {
          label = "Settings",
          fields = {
            crap.fields.relationship({
              name = "parent",
              relationship = { collection = "pages" },
              admin = { description = "Parent page for nested navigation" },
            }),
            crap.fields.select({
              name = "template",
              default_value = "default",
              options = {
                { label = "Default", value = "default" },
                { label = "Full Width", value = "full_width" },
                { label = "Landing", value = "landing" },
                { label = "Sidebar", value = "sidebar" },
              },
            }),
            crap.fields.checkbox({ name = "show_in_nav", default_value = true }),
            crap.fields.number({ name = "nav_order", default_value = 0, admin = { step = "1" } }),
          },
        },
      },
    }),
  },
  hooks = {},
  access = {
    read = "access.anyone",
    create = "access.editor_or_above",
    update = "access.editor_or_above",
    delete = "access.admin_only",
  },
})
