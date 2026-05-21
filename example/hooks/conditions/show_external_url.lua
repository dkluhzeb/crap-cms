--- Client-side display condition: show `external_url` only when
--- `post_type` is either "link" or "video".
return crap.collections.posts.condition(function(_data)
  return { field = "post_type", ["in"] = { "link", "video" } }
end)
