# Custom Strategies

Custom auth strategies let you authenticate users via mechanisms other than password login — API keys, LDAP, SSO headers, etc.

## Configuration

```lua
crap.collections.define("users", {
    auth = {
        strategies = {
            {
                name = "api-key",
                authenticate = "hooks.auth.api_key_check",
            },
            {
                name = "sso",
                authenticate = "hooks.auth.sso_check",
            },
        },
        -- disable_local = true,  -- optionally disable password login
    },
    -- ...
})
```

## Strategy Properties

| Property | Type | Description |
|----------|------|-------------|
| `name` | string | Strategy name for logging and identification |
| `authenticate` | string | Lua function ref in `module.function` format |

## Authenticate Function

The function receives a context table and returns a user document (table) or `nil`/`false`.

```lua
-- hooks/auth.lua
local M = {}

function M.api_key_check(ctx)
    -- ctx.headers   = table of request headers (lowercase keys)
    -- ctx.collection = auth collection slug ("users")

    local key = ctx.headers["x-api-key"]
    if key == nil then return nil end

    -- Look up user by API key
    local result = crap.collections.find(ctx.collection, {
        where = { api_key = key },
        limit = 1,
    })

    if result.total > 0 then
        return result.documents[1]  -- return user document
    end

    return nil  -- strategy didn't match
end

return M
```

## Context Table

| Field | Type | Description |
|-------|------|-------------|
| `headers` | table | HTTP request headers (lowercase keys, string values) |
| `collection` | string | Auth collection slug |

## CRUD Access

Strategy functions have full CRUD access (via the same TxContext pattern as hooks). They can query the database to look up users.

## Execution Order

In admin UI middleware:

1. JWT cookie check (fast path — always runs first)
2. Custom strategies in definition order
3. Redirect to `/admin/login` (if all fail)

## Disabling Password Login

Set `disable_local = true` to hide the password login form:

```lua
auth = {
    disable_local = true,
    strategies = {
        { name = "sso", authenticate = "hooks.auth.sso_check" },
    },
}
```

When `disable_local` is true:
- The login form shows a message instead of email/password inputs
- Only custom strategies can authenticate users
- The `Login` gRPC RPC is effectively disabled for this collection

## Auth Callbacks (OAuth2 / OIDC)

For redirect-based auth flows (OAuth2, OIDC, SAML), use the built-in callback route:

```
GET/POST /admin/auth/callback/{name}
```

This dispatches to a Lua hook `auth_callback.{name}` which receives request headers and query parameters. The hook returns a user document to create a session.

```lua
-- hooks/auth_callback/google.lua
local M = {}

function M.google(ctx)
    -- ctx.headers._query_code contains the OAuth authorization code
    local code = ctx.headers["_query_code"]
    if not code then return nil end

    -- Exchange code for tokens
    local res = crap.http.request({
        method = "POST",
        url = "https://oauth2.googleapis.com/token",
        json = {
            code = code,
            client_id = crap.env.get("GOOGLE_CLIENT_ID"),
            client_secret = crap.env.get("GOOGLE_CLIENT_SECRET"),
            redirect_uri = crap.env.get("GOOGLE_REDIRECT_URI"),
            grant_type = "authorization_code",
        },
    })
    if res.status ~= 200 then return nil end

    local tokens = crap.json.decode(res.body)

    -- Get user info
    local info_res = crap.http.request({
        url = "https://www.googleapis.com/oauth2/v2/userinfo",
        headers = { Authorization = "Bearer " .. tokens.access_token },
    })
    local userinfo = crap.json.decode(info_res.body)

    -- Find or create user
    local users = crap.find("users", { where = { email = userinfo.email } })
    if #users.documents > 0 then return users.documents[1] end

    return crap.create("users", {
        email = userinfo.email,
        name = userinfo.name,
    })
end

return M
```

To initiate the OAuth flow, add a link on your login page pointing to the provider's authorize URL with your `redirect_uri` set to `/admin/auth/callback/google`.

## Email MFA

Auth collections can require a second factor after password verification:

```lua
auth = {
    mfa = "email",  -- "email" or false (default)
}
```

When enabled, after successful password/strategy authentication, a 6-digit code is emailed to the user. They must enter the code to complete login. Codes expire after 5 minutes and are single-use.
