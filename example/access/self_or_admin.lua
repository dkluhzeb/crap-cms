return crap.any.access(function(context)
	if not context.user then
		return false
	end
	if context.user.role == "admin" then
		return true
	end
	-- Users can update/read their own document
	return context.id ~= nil and context.id == context.user.id
end)
