--- Field after_read hook: compute reading time from richtext content.
--- Returns a virtual "X min read" string.
---@param value any
---@param context crap.field_hook.Posts
---@return string
return function(value, context)
  if not value or value == "" then
    return "1 min read"
  end

  -- Strip HTML tags and count words
  local text = tostring(value):gsub("<[^>]+>", " ")
  local word_count = 0
  for _ in text:gmatch("%S+") do
    word_count = word_count + 1
  end

  local minutes = math.max(1, math.ceil(word_count / 200))
  return minutes .. " min read"
end
