--- Hooks for the auth-hook transaction-scope tests.
local M = {}

--- after_change on members: register a commit effect, and fail for "boom"
--- (after the member's row was written in the shared transaction).
function M.after_change(ctx)
    crap.tx.on_commit("hooks.members.log_commit", { name = ctx.data.name })

    if ctx.data.name == "boom" then
        error("boom requested")
    end
end

--- The commit effect: record the committed member through pool-mode CRUD.
function M.log_commit(ctx)
    crap.collections.create("member_log", {
        message = "commit:" .. ctx.data.name,
    }, { override_access = true })
end

--- Callback / strategy: provision the member named by `x-name` on first
--- sign-in, then authenticate — unless `x-deny` is set.
function M.provision(ctx)
    local name = ctx.headers["x-name"]
    local found = crap.collections.find("members", {
        where = { name = name },
        override_access = true,
    })

    if found.pagination.total_docs == 0 then
        crap.collections.create("members", { name = name }, { override_access = true })
    end

    if ctx.headers["x-deny"] then
        return nil
    end

    return { id = "sso-user", email = "sso@x.com" }
end

--- Callback that catches a failed CRUD call and carries on: the failed call
--- must leave nothing behind, and the rest of the hook's writes commit.
function M.provision_after_caught_failure(ctx)
    local ok = pcall(crap.collections.create, "members", { name = "boom" }, {
        override_access = true,
    })
    assert(not ok, "the boom member's after_change raises")

    crap.collections.create("members", { name = ctx.headers["x-name"] }, {
        override_access = true,
    })

    return { id = "sso-user", email = "sso@x.com" }
end

--- `mfa_deliver`: record the delivery as a member.
function M.deliver(ctx)
    crap.collections.create("members", { name = "delivered:" .. ctx.code }, {
        override_access = true,
    })
end

return M
