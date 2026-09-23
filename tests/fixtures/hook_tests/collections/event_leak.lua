--- Regression fixture for the event-stream strip: a read-denied field plus an
--- after_read hook that copies it into an unprotected field (the pipeline must
--- strip BEFORE after_read, so the hook only sees nil), a field only admins may
--- read, and a hidden field — each subscriber's payload is stripped by its own
--- access, from the stored row.
crap.collections.define("event_leak", {
    labels = { singular = "EventLeak", plural = "EventLeaks" },
    fields = {
        { name = "title", type = "text" },
        { name = "summary", type = "text" },
        {
            name = "secret",
            type = "text",
            access = { read = "hooks.event_leak_hooks.deny" },
        },
        {
            name = "notes",
            type = "text",
            access = { read = "hooks.access.check_role" },
        },
        { name = "internal", type = "text", hidden = true },
    },
    hooks = {
        after_read = { "hooks.event_leak_hooks.copy_secret" },
    },
})
