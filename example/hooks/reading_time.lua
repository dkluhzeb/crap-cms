--- Field after_read hook for posts.reading_time (a virtual text field):
--- compute the reading time from the post's `content`.
---
--- The hook is registered on `reading_time`, so its own `value` is that
--- (empty) field — the text comes from the document in `ctx.data`. `content`
--- is a rich text field stored as a JSON document (`format = "json"`), which a
--- read returns as a table; an HTML string is counted too, for content written
--- before the field switched format.

--- Count the words in every text node of a ProseMirror JSON document.
---@param node table
---@return integer
local function count_doc_words(node)
  local count = 0

  if type(node.text) == "string" then
    for _ in node.text:gmatch("%S+") do
      count = count + 1
    end
  end

  for _, child in ipairs(node.content or {}) do
    count = count + count_doc_words(child)
  end

  return count
end

--- Count the words in an HTML string, ignoring its tags.
---@param html string
---@return integer
local function count_html_words(html)
  local count = 0
  local text = html:gsub("<[^>]+>", " ")

  for _ in text:gmatch("%S+") do
    count = count + 1
  end

  return count
end

return crap.collections.posts.field_hook("reading_time", function(_value, ctx)
  local content = ctx.data.content
  local words = 0

  if type(content) == "table" then
    words = count_doc_words(content)
  elseif type(content) == "string" then
    words = count_html_words(content)
  end

  local minutes = math.max(1, math.ceil(words / 200))

  return minutes .. " min read"
end)
