--- Update access for projects: admins and directors may edit any project, a
--- team member only the projects whose stored team includes them.
---
--- Membership is checked on the STORED project, never on `context.data`: an
--- update's `context.data` is the incoming patch (a partial update may not
--- carry `team` at all, and a request could list its sender in it).
---
--- `team` is a has-many relationship, so it cannot be expressed as a filter
--- table (access constraints must reference a flat own column). The rule
--- loads the stored project instead — with `override_access = true`, since a
--- CRUD call from an access function runs with no identity — at `depth = 0`,
--- where a has-many relationship reads as a list of ids.
return crap.any.access(function(context)
  if not context.user then
    return false
  end

  local role = context.user.role
  if role == "admin" or role == "director" then
    return true
  end

  if not context.id then
    return false
  end

  local project = crap.collections.projects.find_by_id(context.id, {
    depth = 0,
    select = { "team" },
    override_access = true,
  })
  if not project or type(project.team) ~= "table" then
    return false
  end

  for _, member_id in ipairs(project.team) do
    if member_id == context.user.id then
      return true
    end
  end

  return false
end)
