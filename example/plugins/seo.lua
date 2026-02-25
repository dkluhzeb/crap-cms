--- SEO plugin — adds meta title, description, social image, and noindex fields
--- to every collection (except upload and auth collections).
---
--- Usage in init.lua:
---   require("plugins.seo").install()

local M = {}

--- The SEO field group added to each collection.
local seo_fields = {
    name = "seo",
    type = "group",
    admin = {
        label = "SEO",
        description = "Search engine optimization settings",
        collapsed = true,
        position = "sidebar",
    },
    fields = {
        {
            name = "meta_title",
            type = "text",
            admin = {
                label = "Meta Title",
                description = "Override the default page title for search engines",
                placeholder = "Custom SEO title...",
            },
        },
        {
            name = "meta_description",
            type = "textarea",
            admin = {
                label = "Meta Description",
                description = "Appears in search result snippets (max 160 chars)",
                placeholder = "Describe this page for search engines...",
            },
        },
        {
            name = "no_index",
            type = "checkbox",
            default_value = false,
            admin = {
                label = "No Index",
                description = "Hide this page from search engines",
            },
        },
    },
}

--- Install the SEO plugin. Adds SEO fields to all content collections.
--- Skips upload collections and auth collections.
--- @param opts? { collections?: string[] }  Optional: limit to specific collection slugs.
function M.install(opts)
    local only = opts and opts.collections

    for slug, def in pairs(crap.collections.config.list()) do
        -- Skip upload and auth collections
        if not def.upload and not def.auth then
            -- If a whitelist was provided, check it
            if not only or crap.util.includes(only, slug) then
                -- Don't add if an "seo" field already exists
                local has_seo = false
                for _, field in ipairs(def.fields) do
                    if field.name == "seo" then
                        has_seo = true
                        break
                    end
                end

                if not has_seo then
                    def.fields[#def.fields + 1] = seo_fields
                    crap.collections.define(slug, def)
                    crap.log.info("seo: added SEO fields to " .. slug)
                end
            end
        end
    end
end

return M
