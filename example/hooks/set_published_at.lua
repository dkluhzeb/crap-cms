--- Collection before_change hook: set published_at when status changes to published.
---@param context crap.HookContext
---@return crap.HookContext
return function(context)
  if not context.data then
    return context
  end

  -- Only set if publishing and no published_at is set
  if context.data._status == "published" and not context.data.published_at then
    context.data.published_at = crap.util.date_now()
  end

  return context
end
