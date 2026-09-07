-- Transaction-outcome effect handlers + registering hooks for the
-- `crap.tx.on_commit` / `on_rollback` integration tests.
local M = {}

-- before_change on tx_articles: register one effect per outcome, then
-- optionally error to force a rollback.
function M.register(ctx)
    crap.tx.on_commit("hooks.effects.log_commit", { title = ctx.data.title })
    crap.tx.on_rollback("hooks.effects.log_rollback", { title = ctx.data.title })
    if ctx.data.boom == "yes" then
        error("boom requested")
    end
    return ctx
end

-- before_change that registers an unresolvable ref (must fail the write).
function M.register_bad_ref(ctx)
    crap.tx.on_commit("hooks.effects.no_such_fn", {})
    return ctx
end

-- before_change registering a failing effect BEFORE a good one — the
-- failure must be logged and skipped, the good effect must still run.
function M.register_failing_effect(ctx)
    crap.tx.on_commit("hooks.effects.explode", {})
    crap.tx.on_commit("hooks.effects.log_commit", { title = ctx.data.title })
    return ctx
end

-- Effect handlers: record the outcome via pool-mode CRUD.
function M.log_commit(ctx)
    crap.collections.tx_log.create({
        message = "commit:" .. (ctx.data.title or "?") .. ":" .. ctx.outcome,
    })
end

function M.log_rollback(ctx)
    crap.collections.tx_log.create({
        message = "rollback:" .. (ctx.data.title or "?") .. ":" .. ctx.outcome,
    })
end

function M.explode(_ctx)
    error("effect exploded")
end

-- after_read that tries to write: after_read has no CRUD, so this must be
-- refused (and, being fail-open, must not break the read).
function M.after_read_writes(ctx)
	crap.collections.tx_log.create({ message = "after-read-write" })
	return ctx
end

return M
