--- Auth collection for the bulk-op password-guard regression tests:
--- `update_many` must reject a `password` key regardless of value type.
crap.collections.define("accounts", {
    auth = true,
    fields = {
        { name = "role", type = "text" },
    },
})
