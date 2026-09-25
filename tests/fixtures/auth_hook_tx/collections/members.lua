--- Accounts provisioned by the auth hooks. Every committed write registers
--- a `crap.tx.on_commit` effect, and a member named "boom" fails its
--- after_change hook after its row was written.
crap.collections.define("members", {
    fields = {
        { name = "name", type = "text", required = true },
    },
    hooks = {
        after_change = { "hooks.members.after_change" },
    },
})
