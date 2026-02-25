crap.collections.define("posts", {
    fields = {
        { name = "title", type = "text", required = true },
        { name = "status", type = "select", options = {
            { label = "Draft", value = "draft" },
            { label = "Published", value = "published" },
        }},
    },
})
