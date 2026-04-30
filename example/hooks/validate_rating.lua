--- before_validate hook for rating fields: enforce 1..5 whole numbers.
---
--- Wired up automatically by `plugins/rating.lua`. Field-level
--- `before_validate` hooks receive `(value, context)` and can either
--- return the value unchanged (or transformed) or `error()` to fail
--- validation with a message surfaced on the field.
---@param value any
---@return number|nil
return function(value)
  if value == nil or value == "" then
    return value
  end

  local n = tonumber(value)
  if not n then
    error("rating must be a number")
  end

  if n < 1 or n > 5 then
    error("rating must be between 1 and 5")
  end

  if n ~= math.floor(n) then
    error("rating must be a whole number")
  end

  return n
end
