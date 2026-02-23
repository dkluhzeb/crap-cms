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
        filters = { api_key = key },
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
