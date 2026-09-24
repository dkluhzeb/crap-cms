--- Collection before_change hook: set published_at the first time a
--- document is published. Used by posts AND projects, so the factory is
--- the generic `crap.any.collection_hook`.
---
--- An update's `context.data` holds only the fields the request sends,
--- so a missing `published_at` there does not mean the document has none:
--- the stored document is read to find out.
return crap.any.collection_hook(function(context)
  -- Only a create or update that publishes (not a draft save, not an
  -- undelete — which carries no field edits) can stamp the date, and never
  -- over a value the request sends itself.
  local writes_fields = context.operation == "create" or context.operation == "update"
  if not writes_fields or context.draft or context.data.published_at ~= nil then
    return context
  end

  if context.operation == "update" then
    local stored = crap.collections.find_by_id(context.collection, context.id, {
      depth = 0,
      select = { "published_at" },
      override_access = true,
    })

    if stored and stored.published_at ~= nil then
      return context
    end
  end

  context.data.published_at = crap.util.date_now()

  return context
end)
