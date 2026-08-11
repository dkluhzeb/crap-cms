-- Public JSON API: list recently published posts.
--
-- Mounted by `crap.routes.register` in init.lua at `GET /blog/posts`.
-- Because the route runs in pool-mode with the same access model as every
-- other read, `crap.collections.find` only returns posts the (anonymous)
-- caller may see — i.e. published ones. Try `GET /blog/posts?limit=5`.
--
-- Wrapping in `crap.any.route_handler` types `ctx` as `crap.RouteContext` for
-- the editor (LuaLS); at runtime it's an identity pass-through.
return crap.any.route_handler(function(ctx)
	local limit = tonumber(ctx.query.limit) or 10
	if limit > 50 then limit = 50 end

	local result = crap.collections.find("posts", {
		limit = limit,
		order_by = "-published_at",
	})

	local posts = {}
	for _, p in ipairs(result.documents) do
		posts[#posts + 1] = {
			title = p.title,
			slug = p.slug,
			excerpt = p.excerpt,
			published_at = p.published_at,
		}
	end

	return {
		json = { posts = posts, total = result.pagination and result.pagination.total_docs or #posts },
		headers = { ["cache-control"] = "public, max-age=60" },
	}
end)
