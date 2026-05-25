return crap.any.access(function(context)
	if not context.user then
		return false
	end
	local role = context.user.role
	if role == "admin" or role == "director" or role == "editor" then
		return true
	end
	-- Authors can update their own content
	if context.data and context.data.author == context.user.id then
		return true
	end
	return false
end)
