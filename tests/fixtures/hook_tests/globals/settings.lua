--- Test global: site settings.
crap.globals.define("settings", {
    labels = { singular = "Settings" },
    fields = {
        { name = "site_name", type = "text" },
        { name = "maintenance_mode", type = "checkbox" },
    },
})
