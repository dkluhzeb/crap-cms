--- Field before_validate hook for projects.budget: enforce
--- non-negative budgets bounded by `CRAP_MAX_BUDGET` env var.
---
--- Per-field overload narrows `value` to `number` (projects.budget's
--- declared field type).
return crap.collections.projects.field_hook("budget", function(value, _context)
  if not value then
    return value
  end

  local limit = tonumber(crap.env.get("CRAP_MAX_BUDGET")) or 500000
  if value > limit then
    error(string.format("Budget cannot exceed %s", limit))
  end

  if value < 0 then
    error("Budget cannot be negative")
  end

  return value
end)
