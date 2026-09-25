--- Drafts-enabled collection whose after_change hook records the `_status`
--- it was handed.
crap.collections.define("journal", {
    labels = { singular = "Entry", plural = "Entries" },
    versions = { drafts = true, max_versions = 0 },
    fields = {
        { name = "title", type = "text", required = true },
    },
    hooks = {
        after_change = { "hooks.status_hooks.record_status" },
    },
})
