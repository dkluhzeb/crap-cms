---
--- Client-side display condition: show `end_date` once the project
--- has moved past the planning stage.
---@param _data crap.data.Projects
---@return table
return function(_data)
  return { field = "status", not_equals = "planning" }
end
