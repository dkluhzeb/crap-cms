crap.collections.define("users", {
  labels = { singular = "User", plural = "Users" },
  timestamps = true,
  auth = {
    forgot_password = true,
    verify_email = false,
    -- mfa = "email",  -- uncomment to enable email-based MFA
    strategies = {
      {
        name = "api-key",
        authenticate = "access.api_key_strategy",
      },
    },
  },
  admin = {
    use_as_title = "name",
    default_sort = "-created_at",
    list_searchable_fields = { "name", "email" },
  },
  fields = {
    crap.fields.text({ name = "name", required = true, admin = { placeholder = "Full name" } }),
    crap.fields.select({
      name = "role",
      required = true,
      default_value = "author",
      options = {
        { label = "Admin", value = "admin" },
        { label = "Director", value = "director" },
        { label = "Editor", value = "editor" },
        { label = "Author", value = "author" },
      },
      admin = { position = "sidebar" },
    }),
    crap.fields.select({
      name = "skills",
      has_many = true,
      options = {
        { label = "Design", value = "design" },
        { label = "Development", value = "development" },
        { label = "Strategy", value = "strategy" },
        { label = "Motion", value = "motion" },
        { label = "Photography", value = "photography" },
        { label = "Copywriting", value = "copywriting" },
        { label = "3D", value = "3d" },
      },
      admin = { description = "Areas of expertise" },
    }),
    crap.fields.upload({ name = "avatar", relationship = { collection = "media" } }),
    crap.fields.textarea({ name = "bio", admin = { rows = 4 } }),
    crap.fields.join({ name = "authored_posts", collection = "posts", on = "author" }),
  },
  hooks = {
    before_delete = { "hooks.prevent_last_admin" },
  },
  access = {
    read = "access.anyone",
    create = "access.admin_only",
    update = "access.self_or_admin",
    delete = "access.admin_only",
  },
})
