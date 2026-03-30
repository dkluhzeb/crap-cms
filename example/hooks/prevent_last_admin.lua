--- Collection before_delete hook: prevent deleting the last admin user.
---@param context crap.HookContext
---@return crap.HookContext
return function(context)
  if not context.data or context.data.role ~= "admin" then
    return context
  end

  ---@type crap.find_result.Users
  local result = crap.collections.find("users", {
    where = { role = "admin" },
    overrideAccess = true,
  })

  local admin_count = result and result.pagination.totalDocs or 0
  if admin_count <= 1 then
    error("Cannot delete the last admin user")
  end

  return context
end
