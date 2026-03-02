--- Field before_validate hook: generate slug from title if empty.
---@param value string|nil
---@param context crap.FieldHookContext
---@return string|nil
return function(value, context)
  if value and value ~= "" then
    return value
  end

  local title = context.data and context.data.title
  if not title or title == "" then
    return value
  end

  return crap.util.slugify(title)
end
