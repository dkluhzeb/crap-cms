--- Test collection with all hook types defined.
crap.collections.define("articles", {
    labels = { singular = "Article", plural = "Articles" },
    fields = {
        { name = "title", type = "text", required = true, unique = true },
        { name = "body", type = "textarea" },
        { name = "status", type = "select", options = {
            { label = "Draft", value = "draft" },
            { label = "Published", value = "published" },
        }},
        { name = "slug", type = "text",
            hooks = {
                before_change = { "hooks.field_hooks.slugify_title" },
            },
        },
        { name = "word_count", type = "number",
            validate = "hooks.validators.positive_number",
        },
    },
    hooks = {
        before_validate = { "hooks.article_hooks.before_validate" },
        before_change = { "hooks.article_hooks.before_change" },
        after_change = { "hooks.article_hooks.after_change" },
        after_read = { "hooks.article_hooks.after_read" },
    },
})
