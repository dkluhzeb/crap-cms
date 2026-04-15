--- Global used by the globals hook lifecycle tests. All three write/read
--- hook phases are wired so tests can assert each fires.
crap.globals.define("site_settings", {
    labels = { singular = "Site Settings" },
    fields = {
        { name = "title", type = "text" },
        { name = "site_name", type = "text" },
        { name = "tagline", type = "text" },
    },
    hooks = {
        before_validate = { "hooks.globals_hooks.before_validate" },
        before_change = { "hooks.globals_hooks.before_change_abort_on_poison" },
        after_read = { "hooks.globals_hooks.uppercase_tagline" },
    },
})
