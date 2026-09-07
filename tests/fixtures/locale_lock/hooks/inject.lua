-- before_change hook that injects a SHARED (non-localized) field into the
-- write. Under a non-default locale the write must be rejected, not silently
-- persisted minus the injected field.
local M = {}

function M.set_slug(ctx)
	ctx.data.slug = "injected"
	return ctx
end

return M
