--- Versioned global whose `update` access returns a filter table — a
--- configuration error on a single-row global that every surface must refuse
--- rather than read as "allowed".
crap.globals.define("scoped_settings", {
    labels = { singular = "Scoped Settings" },
    versions = true,
    fields = {
        { name = "motto", type = "text" },
    },
    access = {
        update = "hooks.access.filter_table",
    },
})
