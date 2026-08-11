--- Regression fixture for event-stream strip ordering: a read-denied field
--- plus an after_read hook that copies it into an unprotected field. The
--- event pipeline must strip BEFORE after_read, so the hook only sees nil.
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
    },
    hooks = {
        after_read = { "hooks.event_leak_hooks.copy_secret" },
    },
})
