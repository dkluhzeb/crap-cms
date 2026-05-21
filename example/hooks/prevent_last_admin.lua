--- Collection before_delete hook: prevent deleting the last admin user.
---@param context crap.HookContext
---@return crap.HookContext
return function(context)
  if not context.data or context.data.role ~= "admin" then
    return context
  end

  -- Per-collection accessor — return type narrows to
  -- `crap.find_result.Users` automatically.
  local result = crap.collections.users.find({
    where = { role = "admin" },
    overrideAccess = true,
  })

  local admin_count = result and result.pagination.totalDocs or 0
  if admin_count <= 1 then
    error("Cannot delete the last admin user")
  end

  return context
end
