crap.collections.define("events", {
  labels = { singular = "Event", plural = "Events" },
  timestamps = true,
  admin = {
    use_as_title = "title",
    default_sort = "-start_date",
    list_searchable_fields = { "title" },
  },
  fields = {
    crap.fields.text({ name = "title", required = true, admin = { placeholder = "Event title" } }),
    crap.fields.text({
      name = "slug",
      required = true,
      unique = true,
      hooks = { before_validate = { "hooks.auto_slug" } },
    }),
    crap.fields.richtext({
      name = "description",
      admin = { features = { "bold", "italic", "link", "bulletList" } },
    }),
    crap.fields.upload({ name = "hero_image", relationship = { collection = "media" } }),
    -- Date row
    crap.fields.row({
      name = "date_row",
      fields = {
        crap.fields.date({
          name = "start_date",
          required = true,
          picker_appearance = "dayAndTime",
          timezone = true,
          admin = { width = "half" },
        }),
        crap.fields.date({
          name = "end_date",
          picker_appearance = "dayAndTime",
          timezone = true,
          admin = { width = "half" },
        }),
      },
    }),
    -- Online toggle + conditional URL
    crap.fields.checkbox({ name = "online", default_value = false }),
    crap.fields.text({
      name = "event_url",
      admin = {
        placeholder = "https://zoom.us/...",
        condition = { field = "online", condition = "equals", value = true },
      },
    }),
    -- Location group
    crap.fields.group({
      name = "location",
      admin = { label = "Venue" },
      fields = {
        crap.fields.text({ name = "venue_name", admin = { placeholder = "Venue name" } }),
        crap.fields.text({ name = "address", admin = { placeholder = "123 Main St" } }),
        crap.fields.text({ name = "city", admin = { width = "half" } }),
        crap.fields.text({ name = "country", admin = { width = "half" } }),
      },
    }),
    -- Speakers (drawer picker)
    crap.fields.relationship({
      name = "speakers",
      relationship = { collection = "users", has_many = true },
      admin = { picker = "drawer", description = "Event speakers / presenters" },
    }),
    crap.fields.relationship({
      name = "categories",
      relationship = { collection = "categories", has_many = true },
    }),
    -- Registration (collapsible)
    crap.fields.collapsible({
      name = "registration",
      admin = { label = "Registration" },
      fields = {
        crap.fields.text({ name = "registration_url", admin = { placeholder = "https://..." } }),
        crap.fields.number({ name = "max_attendees", min = 0, admin = { step = "1" } }),
        crap.fields.date({ name = "registration_deadline", picker_appearance = "dayAndTime" }),
      },
    }),
  },
  access = {
    read = "access.anyone",
    create = "access.editor_or_above",
    update = "access.editor_or_above",
    delete = "access.admin_or_director",
  },
})
