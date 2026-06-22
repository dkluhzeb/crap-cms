--- Field-level read access keyed on the full document: the field is readable
--- only when the document it belongs to is public (`document.public == true`).
--- Used to prove that batched list-read stripping evaluates each document with
--- its OWN `context.document` even though one Lua VM is shared across the batch.
return crap.any.access(function(context)
	return context.document ~= nil and context.document.public == true
end)
