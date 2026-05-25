--- Collection before_delete hook: prevent deleting the last admin
--- user. `before_delete` always receives a generic `crap.HookContext`
--- (per-collection narrowing doesn't apply on delete because
--- `context.data` only carries `{ id = "..." }`), so the factory is
--- `crap.any.collection_hook`.
return crap.any.collection_hook(function(context)
  if not context.data or context.data.role ~= "admin" then
    return context
  end

  local result = crap.collections.users.find({
    where = { role = "admin" },
    overrideAccess = true,
  })

  local admin_count = result and result.pagination.totalDocs or 0
  if admin_count <= 1 then
    error("Cannot delete the last admin user")
  end

  return context
end)
