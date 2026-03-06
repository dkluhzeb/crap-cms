--- Auth strategy test functions.
local M = {}

--- API key auth strategy: checks X-Api-Key header.
function M.api_key_auth(ctx)
    if ctx.headers["x-api-key"] == "valid-key" then
        local result = crap.collections.find("articles", { overrideAccess = true })
        if result.pagination.totalDocs > 0 then
            return result.documents[1]
        end
    end
    return nil
end

return M
