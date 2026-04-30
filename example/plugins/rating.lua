--- Rating field plugin: a 1..5 star widget built on top of the stock
--- `crap.fields.number` — no Rust changes, no custom field-type
--- registry. The "custom-ness" comes from three pieces of glue, all
--- using existing functionality:
---
---   1. A wrapper function that returns a pre-configured number field
---      (this file).
---   2. A field-level `before_validate` hook that enforces 1..5 integer
---      semantics (`hooks/validate_rating.lua`).
---   3. A template overlay (`templates/fields/number.hbs`) that branches
---      on `name == "rating"` to render `<crap-stars>` instead of the
---      standard number input.
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
--- Naming: the template overlay matches on `name == "rating"`, so the
--- wrapper enforces that name. Callers pass other options (required,
--- default_value, admin.description, etc.) through unchanged.
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

  return crap.fields.number({
    -- The template overlay keys off `name = "rating"`. A different name
    -- here means the stock number input renders instead.
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
