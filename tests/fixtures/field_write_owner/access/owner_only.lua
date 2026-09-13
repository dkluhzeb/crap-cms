-- Field update rule: only the document's owner may write the field. It reads
-- the owner from `ctx.document`, which on an update must be the STORED
-- document — otherwise a caller could claim ownership in the same write.
return crap.any.access(function(context)
	return context.user ~= nil
		and context.document ~= nil
		and context.document.owner == context.user.id
end)
