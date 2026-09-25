crap.collections.define("inquiries", {
  labels = { singular = "Inquiry", plural = "Inquiries" },
  timestamps = true,
  admin = {
    use_as_title = "name",
    default_sort = "-created_at",
    list_searchable_fields = { "name", "email", "company" },
  },
  fields = {
    crap.fields.text({ name = "name", required = true, admin = { placeholder = "Full name" } }),
    crap.fields.email({ name = "email", required = true }),
    crap.fields.text({ name = "company" }),
    crap.fields.text({ name = "phone" }),
    crap.fields.relationship({
      name = "service",
      relationship = { collection = "services" },
      admin = { description = "Service they're interested in" },
    }),
    crap.fields.select({
      name = "budget_range",
      options = {
        { label = "Under $5,000", value = "under_5k" },
        { label = "$5,000 - $15,000", value = "5k_15k" },
        { label = "$15,000 - $50,000", value = "15k_50k" },
        { label = "$50,000 - $100,000", value = "50k_100k" },
        { label = "Over $100,000", value = "over_100k" },
      },
    }),
    crap.fields.textarea({ name = "message", required = true, admin = { rows = 6 } }),
    crap.fields.select({
      name = "status",
      required = true,
      default_value = "new",
      options = {
        { label = "New", value = "new" },
        { label = "Contacted", value = "contacted" },
        { label = "Qualified", value = "qualified" },
        { label = "Proposal Sent", value = "proposal" },
        { label = "Won", value = "won" },
        { label = "Lost", value = "lost" },
        { label = "Archived", value = "archived" },
      },
      admin = { position = "sidebar" },
    }),
    crap.fields.relationship({
      name = "assigned_to",
      relationship = { collection = "users" },
      admin = { position = "sidebar" },
    }),
    -- Internal notes (field-level access: admin/director only)
    crap.fields.textarea({
      name = "internal_notes",
      admin = { rows = 4, description = "Internal notes (admin/director only)" },
      access = {
        read = "access.field_admin_or_director",
        create = "access.field_admin_or_director",
        update = "access.field_admin_or_director",
      },
    }),
    -- JSON metadata for tracking
    crap.fields.json({
      name = "metadata",
      admin = {
        description = "Tracking data (UTM params, referrer, etc.)",
      },
    }),
  },
  hooks = {
    after_change = { "hooks.notify_inquiry" },
  },
  access = {
    read = "access.authenticated",
    create = "access.anyone",
    update = "access.editor_or_above",
    delete = "access.admin_only",
  },
})
