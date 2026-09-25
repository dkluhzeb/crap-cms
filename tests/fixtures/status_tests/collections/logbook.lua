--- Drafts-enabled collection without timestamps: its table has no
--- `created_at` / `updated_at` columns.
crap.collections.define("logbook", {
    labels = { singular = "Log", plural = "Logs" },
    timestamps = false,
    versions = { drafts = true, max_versions = 0 },
    fields = {
        { name = "title", type = "text" },
    },
})
