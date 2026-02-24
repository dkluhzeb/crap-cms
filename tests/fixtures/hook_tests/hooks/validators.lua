--- Custom validate functions.
local M = {}

--- Validate that a number is positive.
function M.positive_number(value, ctx)
    if value == nil then return true end
    local n = tonumber(value)
    if n == nil then return "Must be a number" end
    if n < 0 then return "Must be a positive number" end
    return true
end

return M
