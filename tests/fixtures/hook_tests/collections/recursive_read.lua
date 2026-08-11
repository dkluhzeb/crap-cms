--- Regression fixture: a before_read hook that reads its own collection.
--- The read paths must depth-cap this recursion like the write paths do.
crap.collections.define("recursive_read", {
    labels = { singular = "RecursiveRead", plural = "RecursiveReads" },
    fields = {
        { name = "title", type = "text" },
    },
    hooks = {
        before_read = { "hooks.recursive_read_hooks.read_again" },
    },
})
