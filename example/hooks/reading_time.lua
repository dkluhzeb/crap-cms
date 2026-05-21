--- Field after_read hook for posts.content (richtext): compute
--- reading time from the rendered HTML.
---
--- Using the per-field overload narrows `value` to `string` (the
--- richtext field's stored type).
return crap.collections.posts.field_hook("content", function(value, _context)
  if not value or value == "" then
    return "1 min read"
  end

  -- Strip HTML tags and count words
  local text = value:gsub("<[^>]+>", " ")
  local word_count = 0
  for _ in text:gmatch("%S+") do
    word_count = word_count + 1
  end

  local minutes = math.max(1, math.ceil(word_count / 200))
  return minutes .. " min read"
end)
