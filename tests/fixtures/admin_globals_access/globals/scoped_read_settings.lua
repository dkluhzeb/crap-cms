--- Global whose `read` access returns a filter table — a configuration error
--- on a single-row global that every surface must refuse rather than read as
--- "allowed".
crap.globals.define("scoped_read_settings", {
    labels = { singular = "Scoped Read Settings" },
    fields = {
        { name = "motto", type = "text" },
    },
    access = {
        read = "hooks.access.filter_table",
    },
})
