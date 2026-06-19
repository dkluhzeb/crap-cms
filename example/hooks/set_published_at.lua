--- Collection before_change hook: set published_at when status
--- changes to published. Used by posts AND projects, so the factory
--- is the generic `crap.any.collection_hook`.
return crap.any.collection_hook(function(context)
  if not context.data then
    return context
  end

  -- Only set when publishing (not a draft save) and no published_at yet.
  -- The publish/draft intent rides `context.draft`, not the data — the engine
  -- sets the `_status` column during persist, after this hook runs, so
  -- `context.data._status` is nil here.
  if not context.draft and not context.data.published_at then
    context.data.published_at = crap.util.date_now()
  end

  return context
end)
