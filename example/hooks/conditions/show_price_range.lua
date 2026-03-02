--- Server-side display condition: show price_range group when pricing_type is not "custom".
---@param data crap.data.Services
---@return boolean
return function(data)
  return data.pricing_type ~= nil and data.pricing_type ~= "custom"
end
