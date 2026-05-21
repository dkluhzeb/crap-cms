--- Field before_validate hook for projects.budget: enforce
--- non-negative budgets bounded by `CRAP_MAX_BUDGET` env var.
---
--- Per-field overload narrows `value` to `number` (projects.budget's
--- declared field type).
return crap.collections.projects.field_hook("budget", function(value, _context)
  if not value then
    return value
  end

  if value > (tonumber(crap.env.get("CRAP_MAX_BUDGET")) or 500000) then
    error(string.format("Budget cannot exceed %d", value))
  end

  if value < 0 then
    error("Budget cannot be negative")
  end

  return value
end)
