--- Update access for posts: editors and above may edit any post, an author
--- only their own.
---
--- Ownership is checked on the STORED post, never on `context.data`: an
--- update's `context.data` is the incoming patch, so `author = <own id>` in
--- the request would otherwise let an author claim anyone's post. The filter
--- table is enforced against the row being updated.
return crap.any.access(function(context)
  if not context.user then
    return false
  end

  local role = context.user.role
  if role == "admin" or role == "director" or role == "editor" then
    return true
  end

  -- An author may not hand their post to someone else.
  local new_author = context.data and context.data.author
  if new_author ~= nil and new_author ~= context.user.id then
    return false
  end

  return { author = context.user.id }
end)
