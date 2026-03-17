--- Test collection with deeply nested layout wrappers inside arrays.
--- Exercises Array > Tabs > Row validation paths.
crap.collections.define("team", {
    labels = { singular = "Team", plural = "Teams" },
    fields = {
        { name = "name", type = "text", required = true },
        { name = "members", type = "array", fields = {
            { name = "member_tabs", type = "tabs", tabs = {
                {
                    label = "Personal",
                    fields = {
                        { name = "name_row", type = "row", fields = {
                            { name = "first_name", type = "text", required = true },
                            { name = "last_name", type = "text", required = true },
                        }},
                        { name = "email", type = "email" },
                    },
                },
                {
                    label = "Work",
                    fields = {
                        { name = "job_title", type = "text" },
                    },
                },
            }},
        }},
    },
})
