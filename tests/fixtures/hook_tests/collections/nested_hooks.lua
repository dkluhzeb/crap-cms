--- Test collection with hooks on nested (group/row) fields.
crap.collections.define("nested_hooks", {
    labels = { singular = "Nested", plural = "Nested" },
    fields = {
        { name = "seo", type = "group", fields = {
            { name = "title", type = "text",
                hooks = {
                    before_change = { "hooks.field_hooks.trim_value" },
                    after_read = { "hooks.field_hooks.uppercase_value" },
                },
            },
        }},
        { name = "layout", type = "row", fields = {
            { name = "sidebar", type = "text",
                hooks = {
                    before_change = { "hooks.field_hooks.trim_value" },
                },
            },
        }},
    },
})
