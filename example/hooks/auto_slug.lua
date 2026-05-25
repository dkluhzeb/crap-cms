--- Field before_validate hook: generate slug from title if empty.
--- Used across multiple collections (posts, pages, tags, ...) so the
--- factory is the generic `crap.any.field_hook`.
return crap.any.field_hook(function(value, context)
  if value and value ~= "" then
    return value
  end

  local title = context.data and context.data.title
  if not title or title == "" then
    return value
  end

  return crap.util.slugify(title)
end)
