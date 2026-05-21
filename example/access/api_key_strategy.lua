--- Custom auth strategy: API key authentication via X-API-Key header.
--- Verifies HMAC-signed keys against the secret from env.
---
--- Key format on the wire: `<user_id>:<hex_hmac_sha256(user_id, secret)>`
---
--- Registered on the `users` collection via the per-collection
--- `auth_strategy` factory — gives discoverable autocomplete from
--- `crap.collections.users.<TAB>` without changing the typed shape
--- (auth context is uniform across collections).
return crap.collections.users.auth_strategy(function(context)
	local api_key = context.headers["x-api-key"]
	if not api_key then
		return nil
	end

	local secret = crap.env.get("CRAP_API_KEY_SECRET")
	if not secret then
		crap.log.warn("API key strategy: CRAP_API_KEY_SECRET not set")
		return nil
	end

	local user_id, signature = api_key:match("^([^:]+):(.+)$")
	if not user_id or not signature then
		return nil
	end

	-- Compute the expected HMAC and compare in constant time.
	-- Direct `==` on hex strings would short-circuit on first
	-- byte difference and leak signature length / position info.
	local expected = crap.crypto.hmac_sha256(user_id, secret)
	if not crap.crypto.constant_time_eq(expected, signature) then
		return nil
	end

	local user = crap.collections.find_by_id(context.collection, user_id, { overrideAccess = true })
	if not user then
		return nil
	end

	return user
end)
