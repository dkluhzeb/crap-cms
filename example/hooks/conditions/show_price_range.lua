--- Server-side display condition: show price_range group when
--- pricing_type is not "custom". Per-collection factory narrows
--- `data` to `crap.data.Services`.
return crap.collections.services.condition(function(data)
  return data.pricing_type ~= nil and data.pricing_type ~= "custom"
end)
