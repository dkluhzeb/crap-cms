--- Field-level hooks.
local M = {}

--- before_change hook for the slug field: generate slug from title.
function M.slugify_title(value, ctx)
    -- If no slug provided, generate from title
    if value == nil or value == "" then
        if ctx.data and ctx.data.title then
            local s = ctx.data.title:lower()
            s = s:gsub("[^%w%s-]", "")
            s = s:gsub("%s+", "-")
            s = s:gsub("-+", "-")
            s = s:match("^%-*(.-)%-*$") or ""
            return s
        end
    end
    return value
end

return M
