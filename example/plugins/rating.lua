--- Rating field plugin: a 1..5 star widget built on top of the stock
--- `crap.fields.number`. Three pieces of glue, all using shipped
--- mechanisms — no Rust changes, no fragile global template overrides:
---
---   1. A wrapper function that returns a pre-configured number field
---      with `admin.template = "fields/rating"` (this file).
---   2. A field-level `before_validate` hook that enforces 1..5 integer
---      semantics (`hooks/validate_rating.lua`).
---   3. A per-field template at `templates/fields/rating.hbs` that
---      renders `<crap-stars>` instead of a plain number input — invoked
---      ONLY for fields opted in via `admin.template`, so it doesn't
---      affect any other `number` fields.
---
--- Use it in a collection like:
---
---   local rating = require("plugins.rating")
---
---   crap.collections.define("testimonials", {
---     fields = {
---       rating.field({ name = "rating", required = true }),
---       ...
---     },
---   })
---
--- Naming: the field can be called anything — the rendering template is
--- selected by `admin.template`, not by name-matching. Multiple
--- different rating-shaped fields with different names work side by
--- side; deeply nested fields work correctly because no name match is
--- happening.
local M = {}

--- Build a rating field config.
---@param opts { name: string?, required: boolean?, default_value: integer?, admin: table? }
---@return table
function M.field(opts)
  opts = opts or {}
  local admin = opts.admin or {}

  if admin.description == nil then
    admin.description = "Rating from 1 to 5"
  end

  -- Per-instance template binding — render this field with
  -- templates/fields/rating.hbs instead of the default fields/number.
  -- `admin.extra` carries config the rating template reads via
  -- `{{admin.extra.<key>}}` so the same template + JS component can
  -- power multiple rating-shaped fields with different settings.
  admin.template = "fields/rating"
  admin.extra = admin.extra or {}
  admin.extra.color = admin.extra.color or "amber"

  return crap.fields.number({
    name = opts.name or "rating",
    required = opts.required,
    min = 1,
    max = 5,
    default_value = opts.default_value or 5,
    -- Field-level hook so every rating instance validates the same way
    -- without callers remembering to wire it up.
    hooks = { before_validate = { "hooks.validate_rating" } },
    admin = admin,
  })
end

return M
