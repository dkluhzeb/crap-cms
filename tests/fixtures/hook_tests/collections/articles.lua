--- Test collection with all hook types defined.
crap.collections.define("articles", {
    labels = { singular = "Article", plural = "Articles" },
    fields = {
        { name = "title", type = "text", required = true, unique = true,
            hooks = {
                before_validate = { "hooks.field_hooks.trim_value" },
                after_read = { "hooks.field_hooks.uppercase_value" },
                after_change = { "hooks.field_hooks.after_change_marker" },
            },
        },
        { name = "body", type = "textarea" },
        { name = "status", type = "select", options = {
            { label = "Draft", value = "draft" },
            { label = "Published", value = "published" },
            { label = "Archived", value = "archived" },
            { label = "Active", value = "active" },
            { label = "Red", value = "red" },
            { label = "Blue", value = "blue" },
            { label = "Green", value = "green" },
            { label = "True", value = "true" },
            { label = "False", value = "false" },
        }},
        { name = "slug", type = "text",
            hooks = {
                before_change = { "hooks.field_hooks.slugify_title" },
            },
        },
        { name = "word_count", type = "number",
            validate = "hooks.validators.positive_number",
        },
        { name = "published_at", type = "date" },
        { name = "event_at", type = "date", picker_appearance = "dayAndTime" },
    },
    hooks = {
        before_validate = { "hooks.article_hooks.before_validate" },
        before_change = { "hooks.article_hooks.before_change" },
        after_change = { "hooks.article_hooks.after_change" },
        after_read = { "hooks.article_hooks.after_read" },
    },
})
