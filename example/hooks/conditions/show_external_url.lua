---
--- Client-side display condition: show `external_url` only when
--- `post_type` is either "link" or "video".
---@param _data crap.data.Posts
---@return table
return function(_data)
  return { field = "post_type", ["in"] = { "link", "video" } }
end
