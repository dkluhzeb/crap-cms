--- Global guarded by admin-only read + update access. Drives HTTP + gRPC
--- access-denied regression tests.
crap.globals.define("restricted_settings", {
    labels = { singular = "Restricted Settings" },
    fields = {
        { name = "secret_value", type = "text" },
    },
    access = {
        read = "hooks.access.admin_only",
        update = "hooks.access.admin_only",
    },
})
