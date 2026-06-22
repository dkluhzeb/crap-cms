--- Field-level read access keyed on the collection slug: the field is readable
--- only inside the `posts` collection. Used to prove that field-access functions
--- now receive the real `context.collection` (previously the empty string).
return crap.any.access(function(context)
	return context.collection == "posts"
end)
