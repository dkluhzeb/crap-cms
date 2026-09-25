--- Drafts-enabled global whose after_change hook records the `_status` it
--- was handed; `seo` is a group for the partial-draft regression.
crap.globals.define("notice", {
    labels = { singular = "Notice" },
    versions = { drafts = true, max_versions = 0 },
    fields = {
        { name = "headline", type = "text" },
        {
            name = "seo",
            type = "group",
            fields = {
                { name = "title", type = "text" },
                { name = "desc", type = "text" },
            },
        },
    },
    hooks = {
        after_change = { "hooks.status_hooks.record_status" },
    },
})
