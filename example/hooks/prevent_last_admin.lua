--- Collection before_delete hook: prevent deleting the last admin
--- user. A delete hook's `context.data` is the full stored document plus
--- `id` (and `soft_delete` on a soft delete), not the write shape the
--- per-collection `crap.hook.Users` describes, so the factory is the
--- generic `crap.any.collection_hook`.
return crap.any.collection_hook(function(context)
  if not context.data or context.data.role ~= "admin" then
    return context
  end

  local result = crap.collections.users.find({
    where = { role = "admin" },
    override_access = true,
  })

  local admin_count = result and result.pagination.total_docs or 0
  if admin_count <= 1 then
    error("Cannot delete the last admin user")
  end

  return context
end)
