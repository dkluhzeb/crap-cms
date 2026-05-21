--- Access for project team members or admins/directors.
return crap.any.access(function(context)
	if not context.user then
		return false
	end
	local role = context.user.role
	if role == "admin" or role == "director" then
		return true
	end
	-- Check if user is on the project team
	if context.data and context.data.team then
		for _, member_id in ipairs(context.data.team) do
			if member_id == context.user.id then
				return true
			end
		end
	end
	return false
end)
