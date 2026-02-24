--- Test collection with rich access control for overrideAccess tests.
crap.collections.define("items", {
    labels = { singular = "Item", plural = "Items" },
    fields = {
        { name = "title", type = "text", required = true },
        { name = "owner", type = "text" },
        { name = "status", type = "select", options = {
            { label = "Draft", value = "draft" },
            { label = "Published", value = "published" },
        }, access = {
            update = "hooks.access_rules.admin_only",
        }},
        { name = "notes", type = "text", access = {
            read = "hooks.access_rules.admin_only",
            create = "hooks.access_rules.admin_only",
            update = "hooks.access_rules.admin_only",
        }},
    },
    access = {
        read = "hooks.access_rules.own_or_admin",
        create = "hooks.access_rules.authenticated",
        update = "hooks.access_rules.authenticated",
        delete = "hooks.access_rules.admin_only",
    },
})
