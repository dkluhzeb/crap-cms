crap.collections.define("articles", {
    labels = {
        singular = "Article",
        plural = "Articles",
    },
    timestamps = true,
    admin = {
        use_as_title = "title",
        default_sort = "-created_at",
        list_searchable_fields = { "title", "summary" },
    },
    versions = {
        drafts = true,
        max_versions = 20,
    },
    fields = {
        {
            name = "title",
            type = "text",
            required = true,
            admin = {
                placeholder = "Enter article title",
            },
        },
        {
            name = "slug",
            type = "text",
            required = true,
            unique = true,
            admin = {
                placeholder = "article-slug",
            },
        },
        {
            name = "summary",
            type = "textarea",
            admin = {
                placeholder = "Brief summary of the article",
            },
        },
        {
            name = "body",
            type = "richtext",
            admin = {
                placeholder = "Write the article body...",
            },
        },
        {
            name = "category",
            type = "select",
            options = {
                { label = "News", value = "news" },
                { label = "Tutorial", value = "tutorial" },
                { label = "Opinion", value = "opinion" },
            },
        },
        {
            name = "author",
            type = "relationship",
            relationship = {
                collection = "users",
                has_many = false,
            },
        },
    },
    hooks = {
        before_change = { "hooks.posts.auto_slug" },
    },
    access = {
        read   = "hooks.access.public_read",
        create = "hooks.access.authenticated",
        update = "hooks.access.authenticated",
        delete = "hooks.access.admin_only",
    },
})
