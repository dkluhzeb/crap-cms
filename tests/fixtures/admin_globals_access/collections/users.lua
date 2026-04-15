--- Users auth collection with a role field so tests can create admin and
--- editor users to exercise the admin-only access gate.
crap.collections.define("users", {
    labels = { singular = "User", plural = "Users" },
    timestamps = true,
    auth = { enabled = true },
    fields = {
        { name = "email", type = "email", required = true, unique = true },
        { name = "name",  type = "text" },
        { name = "role",  type = "text" },
    },
})
