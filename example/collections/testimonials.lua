-- Custom field wrapper: see plugins/rating.lua. Demonstrates how to
-- ship a "custom field" using only existing functionality —
-- a Lua wrapper, a `before_validate` hook, and a template overlay
-- that branches on field name.
local rating = require("plugins.rating")

crap.collections.define("testimonials", {
  labels = { singular = "Testimonial", plural = "Testimonials" },
  timestamps = true,
  admin = {
    use_as_title = "author_name",
    default_sort = "-created_at",
    list_searchable_fields = { "author_name", "company" },
  },
  fields = {
    crap.fields.text({ name = "author_name", required = true, admin = { placeholder = "Client name" } }),
    crap.fields.text({ name = "author_title", admin = { placeholder = "CEO, Acme Corp" } }),
    crap.fields.text({ name = "company" }),
    crap.fields.upload({ name = "author_photo", relationship = { collection = "media" } }),
    crap.fields.textarea({
      name = "quote",
      required = true,
      admin = { rows = 4, placeholder = "What the client said..." },
    }),
    rating.field({ name = "rating", required = true }),
    crap.fields.relationship({
      name = "project",
      relationship = { collection = "projects" },
      admin = { description = "Related project" },
    }),
    crap.fields.checkbox({ name = "featured", default_value = false, admin = { position = "sidebar" } }),
  },
  access = {
    read = "access.anyone",
    create = "access.editor_or_above",
    update = "access.editor_or_above",
    delete = "access.admin_only",
  },
})
