--- Versioned without drafts: an audit trail with no unpublished state.
crap.collections.define("audit_log", {
    labels = { singular = "Audit entry", plural = "Audit entries" },
    versions = { drafts = false, max_versions = 0 },
    fields = {
        { name = "title", type = "text" },
    },
})
