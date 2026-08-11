--- Soft-delete collection for the delete_many trash-emptying parity test.
crap.collections.define("trashable", {
    labels = { singular = "Trashable", plural = "Trashables" },
    soft_delete = true,
    fields = {
        { name = "title", type = "text" },
    },
})
