--- Auth strategy test functions.
local M = {}

--- API key auth strategy: checks X-Api-Key header.
function M.api_key_auth(ctx)
    if ctx.headers["x-api-key"] == "valid-key" then
        local result = crap.collections.find("articles", { override_access = true })
        if result.pagination.total_docs > 0 then
            return result.documents[1]
        end
    end
    return nil
end

--- Credential strategy: verifies the submitted email + password reach the
--- hook (the gRPC/admin form login passes them via ctx.email / ctx.password).
function M.credential_auth(ctx)
    if ctx.email == "admin@x.com" and ctx.password == "secret" then
        return { id = "cred-user", email = ctx.email }
    end
    return nil
end

--- Strategy with a side-effect write: creates an article, then
--- authenticates only when x-succeed is set. Used to pin the
--- transaction contract (commit on success, rollback otherwise).
function M.writing_auth(ctx)
    crap.collections.create("articles", {
        title = "From Strategy",
        body = "side effect",
    }, { override_access = true })

    if ctx.headers["x-succeed"] == "yes" then
        return { id = "strategy-user", email = "s@x.com" }
    end
    return nil
end

return M
