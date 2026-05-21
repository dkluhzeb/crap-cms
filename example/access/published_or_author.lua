--- Read access: admins see all, authenticated see all, anonymous see published only.
return crap.any.access(function(context)
	if context.user then
		local role = context.user.role
		if role == "admin" or role == "director" or role == "editor" then
			return true
		end
		-- Authenticated users (authors) can see all posts
		-- (no OR support in access filters, so we allow read and rely on
		-- update/delete access to protect editing)
		return true
	end
	-- Anonymous: published only
	return { _status = "published" }
end)
