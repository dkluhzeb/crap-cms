--- Client-side display condition: show `end_date` once the project
--- has moved past the planning stage.
return crap.collections.projects.condition(function(_data)
  return { field = "status", not_equals = "planning" }
end)
