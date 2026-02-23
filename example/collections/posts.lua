crap.collections.define("posts", {
    labels = {
        singular = "Post",
        plural = "Posts",
    },
    timestamps = true,
    admin = {
        use_as_title = "title",
        default_sort = "-created_at",
        list_searchable_fields = { "title", "slug", "content" },
    },
    fields = {
        {
            name = "title",
            type = "text",
            required = true,
            hooks = {
                before_validate = { "hooks.posts.trim_title" },
            },
            admin = {
                placeholder = "Enter post title",
            },
        },
        {
            name = "slug",
            type = "text",
            required = true,
            unique = true,
            admin = {
                placeholder = "post-slug",
            },
        },
        {
            name = "status",
            type = "select",
            required = true,
            default_value = "draft",
            options = {
                { label = "Draft", value = "draft" },
                { label = "Published", value = "published" },
                { label = "Archived", value = "archived" },
            },
            access = {
                update = "hooks.access.admin_only",
            },
        },
        {
            name = "content",
            type = "richtext",
            admin = {
                placeholder = "Write your post content...",
            },
        },
        {
            name = "tags",
            type = "relationship",
            relationship = {
                collection = "tags",
                has_many = true,
            },
            admin = {
                description = "Select tags for this post",
            },
        },
        {
            name = "slides",
            type = "array",
            fields = {
                {
                    name = "title",
                    type = "text",
                    required = true,
                },
                {
                    name = "image_url",
                    type = "text",
                },
                {
                    name = "caption",
                    type = "textarea",
                },
            },
            admin = {
                description = "Image slides for the post gallery",
            },
        },
        {
            name = "image",
            type = "relationship",
            relationship = {
                collection = "media",
                has_many = false,
            },
            admin = {
                description = "Featured image for this post",
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
