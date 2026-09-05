-- Localized collection for the export/import round-trip tests.
crap.collections.define("articles", {
    fields = {
        { name = "title", type = "text", localized = true },
        { name = "body", type = "textarea" },
    },
})
