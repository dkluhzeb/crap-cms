--- Custom auth strategy: API key authentication via X-API-Key header.
--- Uses crap.crypto.hmac to verify HMAC-signed keys against the secret from env.
---@param context crap.AuthStrategyContext
---@return crap.Document?
return function(context)
	local api_key = context.headers["x-api-key"]
	if not api_key then
		return nil
	end

	local secret = crap.env.get("CRAP_API_KEY_SECRET")
	if not secret then
		crap.log.warn("API key strategy: CRAP_API_KEY_SECRET not set")
		return nil
	end

	-- Key format: <user_id>:<hmac_signature>
	local user_id, signature = api_key:match("^([^:]+):(.+)$")
	if not user_id or not signature then
		return nil
	end

	-- Verify HMAC
	local expected = crap.crypto.hmac("sha256", secret, user_id)
	if expected ~= signature then
		return nil
	end

	-- Look up user
	local user = crap.collections.find_by_id(context.collection, user_id, { overrideAccess = true })
	if not user then
		return nil
	end

	return user
end
