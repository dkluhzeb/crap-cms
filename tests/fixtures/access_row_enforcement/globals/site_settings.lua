--- Global with an access hook that wrongly returns a filter table — used to
--- exercise the "globals reject Constrained" path for both read and update.
crap.globals.define("site_settings", {
    labels = { singular = "Site Settings" },
    fields = {
        { name = "site_name", type = "text", required = true },
    },
    access = {
        read = "hooks.row_rules.own_rows",
        update = "hooks.row_rules.own_rows",
    },
})
