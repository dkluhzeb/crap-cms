--- Global guarded by an admin-only access hook. Used by access-denied tests.
crap.globals.define("restricted", {
    labels = { singular = "Restricted" },
    fields = {
        { name = "secret_value", type = "text" },
    },
    access = {
        read = "hooks.globals_hooks.admin_only",
        update = "hooks.globals_hooks.admin_only",
    },
})
