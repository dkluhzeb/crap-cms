--- Read access for posts.
---
--- The `read` view is published-only *by design* — the system composes
--- `_status = 'published'` for it — so anyone allowed here sees only published
--- posts. Draft visibility is a separate concern, gated by the collection's
--- `draft` access key (which falls back to `update`); it is never expressed by
--- returning a `_status` constraint from a read hook (operators never write
--- `_status`).
---
--- So this hook only decides *who* may read published posts: everyone.
return crap.any.access(function(context)
	return true
end)
