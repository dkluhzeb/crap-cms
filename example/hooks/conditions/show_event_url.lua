--- Client-side display condition: show `event_url` only when the
--- event is online.
return crap.collections.events.condition(function(_data)
  return { field = "online", equals = true }
end)
