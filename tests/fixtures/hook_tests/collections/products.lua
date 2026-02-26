crap.collections.define("products", {
    labels = { singular = "Product", plural = "Products" },
    fields = {
        { name = "name", type = "text", required = true },
        -- Group field (parent table, expanded columns)
        { name = "seo", type = "group", fields = {
            { name = "meta_title", type = "text" },
        }},
        -- Array field with scalar + group sub-fields
        { name = "variants", type = "array", fields = {
            { name = "color", type = "text" },
            { name = "dimensions", type = "group", fields = {
                { name = "width", type = "text" },
                { name = "height", type = "text" },
            }},
        }},
        -- Blocks field with group-in-block
        { name = "content", type = "blocks", blocks = {
            { type = "text", label = "Text", fields = {
                { name = "body", type = "textarea" },
            }},
            { type = "section", label = "Section", fields = {
                { name = "heading", type = "text" },
                { name = "meta", type = "group", fields = {
                    { name = "author", type = "text" },
                }},
            }},
        }},
    },
})
