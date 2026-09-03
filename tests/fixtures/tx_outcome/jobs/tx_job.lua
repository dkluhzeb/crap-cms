-- Job handlers driving crap.transaction + crap.tx from pool-mode.
local M = {}

-- Commit path: a create plus both outcome registrations inside one tx.
function M.run_commit(_ctx)
    crap.transaction(function()
        crap.collections.tx_articles.create({ title = "in-tx" })
        crap.tx.on_commit("hooks.effects.log_commit", { title = "job" })
        crap.tx.on_rollback("hooks.effects.log_rollback", { title = "job" })
    end)
    return { ok = true }
end

-- Rollback path: register both, then error inside the transaction.
function M.run_rollback(_ctx)
    local ok = pcall(crap.transaction, function()
        crap.collections.tx_articles.create({ title = "doomed" })
        crap.tx.on_commit("hooks.effects.log_commit", { title = "job" })
        crap.tx.on_rollback("hooks.effects.log_rollback", { title = "job" })
        error("forced")
    end)
    return { ok = ok }
end

-- Outside any transaction: registration must raise.
function M.run_no_tx(_ctx)
    local ok, err = pcall(function()
        crap.tx.on_commit("hooks.effects.log_commit", {})
    end)
    return { ok = ok, err = tostring(err) }
end

return M
