-- Test collection exercising every layout wrapper nesting combination.
-- Tabs, Row, and Collapsible inside Array and Blocks sub-fields,
-- including nested layouts (Row inside Tabs, Tabs inside Collapsible, etc.).

crap.collections.define("test_nesting", {
  labels = { singular = "Nesting Test", plural = "Nesting Tests" },
  timestamps = true,
  admin = {
    use_as_title = "name",
    default_sort = "-created_at",
  },
  fields = {
    crap.fields.text({ name = "name", required = true }),

    -- ─── Array with Tabs sub-fields ─────────────────────────────────────
    -- Each array row has fields organized into tabs.
    -- Data is stored flat in the join table (title, description, color columns).
    crap.fields.array({
      name = "tabbed_items",
      admin = { labels_singular = "Tabbed Item" },
      fields = {
        crap.fields.tabs({
          name = "item_tabs",
          tabs = {
            {
              label = "Content",
              fields = {
                crap.fields.text({ name = "title", required = true }),
                crap.fields.textarea({ name = "description" }),
              },
            },
            {
              label = "Appearance",
              fields = {
                crap.fields.select({
                  name = "color",
                  options = {
                    { label = "Red", value = "red" },
                    { label = "Blue", value = "blue" },
                    { label = "Green", value = "green" },
                  },
                }),
                crap.fields.checkbox({ name = "featured" }),
              },
            },
          },
        }),
      },
    }),

    -- ─── Array with Row sub-fields ──────────────────────────────────────
    -- Row groups fields horizontally in the admin UI.
    crap.fields.array({
      name = "coordinates",
      admin = { labels_singular = "Point" },
      fields = {
        crap.fields.row({
          name = "coord_row",
          fields = {
            crap.fields.number({ name = "x", required = true, admin = { width = "33%" } }),
            crap.fields.number({ name = "y", required = true, admin = { width = "33%" } }),
            crap.fields.number({ name = "z", admin = { width = "33%" } }),
          },
        }),
        crap.fields.text({ name = "label" }),
      },
    }),

    -- ─── Array with Collapsible sub-fields ──────────────────────────────
    crap.fields.array({
      name = "faq_items",
      admin = { labels_singular = "FAQ" },
      fields = {
        crap.fields.text({ name = "question", required = true }),
        crap.fields.collapsible({
          name = "answer_section",
          admin = { label = "Answer Details" },
          fields = {
            crap.fields.richtext({ name = "answer" }),
            crap.fields.text({ name = "source_url", admin = { placeholder = "https://..." } }),
          },
        }),
      },
    }),

    -- ─── Array with Row inside Tabs (double nesting) ────────────────────
    -- Tabs containing Rows — tests recursive layout flattening.
    crap.fields.array({
      name = "team_members",
      admin = { labels_singular = "Member", label_field = "first_name" },
      fields = {
        crap.fields.tabs({
          name = "member_tabs",
          tabs = {
            {
              label = "Personal",
              fields = {
                crap.fields.row({
                  name = "name_row",
                  fields = {
                    crap.fields.text({ name = "first_name", required = true, admin = { width = "50%" } }),
                    crap.fields.text({ name = "last_name", required = true, admin = { width = "50%" } }),
                  },
                }),
                crap.fields.email({ name = "email" }),
              },
            },
            {
              label = "Professional",
              fields = {
                crap.fields.text({ name = "job_title" }),
                crap.fields.row({
                  name = "social_row",
                  fields = {
                    crap.fields.text({ name = "linkedin", admin = { width = "50%", placeholder = "LinkedIn URL" } }),
                    crap.fields.text({ name = "github", admin = { width = "50%", placeholder = "GitHub URL" } }),
                  },
                }),
              },
            },
          },
        }),
      },
    }),

    -- ─── Blocks with Tabs sub-fields ────────────────────────────────────
    -- Block type whose fields are organized with tabs.
    crap.fields.blocks({
      name = "sections",
      admin = { labels_singular = "Section" },
      blocks = {
        {
          type = "feature_card",
          label = "Feature Card",
          fields = {
            crap.fields.tabs({
              name = "card_tabs",
              tabs = {
                {
                  label = "Content",
                  fields = {
                    crap.fields.text({ name = "heading", required = true }),
                    crap.fields.textarea({ name = "body" }),
                  },
                },
                {
                  label = "Style",
                  fields = {
                    crap.fields.select({
                      name = "variant",
                      default_value = "default",
                      options = {
                        { label = "Default", value = "default" },
                        { label = "Highlighted", value = "highlighted" },
                        { label = "Minimal", value = "minimal" },
                      },
                    }),
                    crap.fields.text({ name = "icon", admin = { placeholder = "Icon name" } }),
                  },
                },
              },
            }),
          },
        },
        {
          type = "stats_row",
          label = "Stats Row",
          fields = {
            -- Row inside a block
            crap.fields.row({
              name = "stats",
              fields = {
                crap.fields.text({ name = "stat_1_label", admin = { width = "25%" } }),
                crap.fields.text({ name = "stat_1_value", admin = { width = "25%" } }),
                crap.fields.text({ name = "stat_2_label", admin = { width = "25%" } }),
                crap.fields.text({ name = "stat_2_value", admin = { width = "25%" } }),
              },
            }),
          },
        },
        {
          type = "accordion",
          label = "Accordion",
          fields = {
            crap.fields.text({ name = "section_title" }),
            -- Collapsible inside a block
            crap.fields.collapsible({
              name = "advanced_opts",
              admin = { label = "Advanced Options" },
              fields = {
                crap.fields.checkbox({ name = "open_first", default_value = true }),
                crap.fields.checkbox({ name = "allow_multiple" }),
                crap.fields.select({
                  name = "animation",
                  default_value = "slide",
                  options = {
                    { label = "Slide", value = "slide" },
                    { label = "Fade", value = "fade" },
                    { label = "None", value = "none" },
                  },
                }),
              },
            }),
          },
        },
      },
    }),

    -- ─── Top-level nested layout wrappers ───────────────────────────────
    -- Tests the versions.rs recursive fix (Row inside Tabs at collection root).
    crap.fields.tabs({
      name = "settings_tabs",
      tabs = {
        {
          label = "Display",
          fields = {
            crap.fields.row({
              name = "display_row",
              fields = {
                crap.fields.select({
                  name = "theme",
                  default_value = "light",
                  options = {
                    { label = "Light", value = "light" },
                    { label = "Dark", value = "dark" },
                  },
                  admin = { width = "50%" },
                }),
                crap.fields.select({
                  name = "layout",
                  default_value = "grid",
                  options = {
                    { label = "Grid", value = "grid" },
                    { label = "List", value = "list" },
                  },
                  admin = { width = "50%" },
                }),
              },
            }),
          },
        },
        {
          label = "Advanced",
          fields = {
            crap.fields.collapsible({
              name = "advanced_settings",
              admin = { label = "Advanced Settings" },
              fields = {
                crap.fields.json({ name = "custom_config", admin = { description = "Raw JSON config" } }),
                crap.fields.code({ name = "custom_css", admin = { language = "css" } }),
              },
            }),
          },
        },
      },
    }),
  },
})
