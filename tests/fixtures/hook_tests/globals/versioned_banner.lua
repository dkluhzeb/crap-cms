--- Versioned global for draft / unpublish parity tests.
crap.globals.define("versioned_banner", {
    labels = { singular = "Banner" },
    versions = {
        drafts = true,
        max_versions = 0,
    },
    fields = {
        { name = "headline", type = "text" },
    },
})
