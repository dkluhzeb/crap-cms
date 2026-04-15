--- Articles collection with filter-returning access hooks for row-enforcement tests.
crap.collections.define("articles", {
    labels = { singular = "Article", plural = "Articles" },
    soft_delete = true,
    fields = {
        { name = "title", type = "text", required = true },
        { name = "author_id", type = "text" },
    },
    access = {
        read = "hooks.row_rules.authenticated",
        create = "hooks.row_rules.authenticated",
        update = "hooks.row_rules.own_rows",
        delete = "hooks.row_rules.own_rows",
        trash = "hooks.row_rules.own_rows",
    },
})

--- Versioned articles — same author-row gating on read/update so the version
--- list and restore paths can enforce Constrained against the parent doc.
crap.collections.define("versioned_articles", {
    labels = { singular = "VersionedArticle", plural = "VersionedArticles" },
    versions = {
        drafts = true,
        max_versions = 0,
    },
    fields = {
        { name = "title", type = "text", required = true },
        { name = "author_id", type = "text" },
    },
    access = {
        read = "hooks.row_rules.own_rows",
        create = "hooks.row_rules.authenticated",
        update = "hooks.row_rules.own_rows",
    },
})

--- Same shape, but with a create hook that wrongly returns a filter table.
crap.collections.define("bad_create_articles", {
    labels = { singular = "BadCreateArticle", plural = "BadCreateArticles" },
    fields = {
        { name = "title", type = "text", required = true },
        { name = "author_id", type = "text" },
    },
    access = {
        read = "hooks.row_rules.authenticated",
        create = "hooks.row_rules.create_returns_filter",
        update = "hooks.row_rules.authenticated",
        delete = "hooks.row_rules.authenticated",
    },
})
