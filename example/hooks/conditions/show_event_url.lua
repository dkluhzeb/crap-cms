---
--- Client-side display condition: show `event_url` only when the
--- event is online.
---@param _data crap.data.Events
---@return table
return function(_data)
  return { field = "online", equals = true }
end
