--- Field before_validate hook: validate budget against max from config.
---@param value number|nil
---@param context crap.field_hook.Inquiries
---@return number|nil
return function(value, context)
  if not value then
    return value
  end

  local max_budget = tonumber(crap.env.get("CRAP_MAX_BUDGET")) or 500000
  if value > max_budget then
    error(string.format("Budget cannot exceed %d", max_budget))
  end

  if value < 0 then
    error("Budget cannot be negative")
  end

  return value
end
