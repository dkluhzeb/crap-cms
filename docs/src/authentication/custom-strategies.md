# Custom Strategies

Custom auth strategies let you authenticate users via mechanisms other than password login — API keys, LDAP, SSO headers, etc. They are one variant of the `auth.methods` list (see [Auth Methods](auth-methods.md) for the overall model).

## Configuration

```lua
crap.collections.define("users", {
    auth = {
        enabled = true,
        methods = crap.auth.with_defaults({
            -- API-key strategy: only fires when `x-api-key` header is present.
            { type = "strategy",
              name = "api-key",
              authenticate = "hooks.auth.api_key_check",
              activates_on = { header = "x-api-key" },
              surfaces = { "grpc", "admin" } },
            -- SSO strategy: only fires when the SSO assertion header is present.
            { type = "strategy",
              name = "sso",
              authenticate = "hooks.auth.sso_check",
              activates_on = { header = "x-sso-assertion" },
              surfaces = { "admin" } },
        }),
    },
    -- ...
})
```

To disable password login entirely (machine-to-machine collection), omit `password_login` from the methods list:

```lua
auth = {
    enabled = true,
    methods = {
        { type = "strategy",
          name = "svc-key",
          authenticate = "hooks.auth.svc_key",
          activates_on = { header = "x-service-key" },
          surfaces = { "grpc" } },
    },
}
```

## Strategy Properties

| Property | Type | Description |
|----------|------|-------------|
| `type` | `"strategy"` | Discriminator. Required. |
| `name` | string | Strategy name for logging and identification. |
| `authenticate` | string | Lua function ref in `module.function` format. |
| `activates_on` | table | When the strategy fires: `{ header = "x-..." }` or `{ always = true }`. Required. |
| `surfaces` | string[] | Host transports the strategy is allowed on (default: `{"admin"}`). |

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
| `headers` | table | HTTP request headers (lowercase keys, string values). On the gRPC login path these are the request metadata entries. |
| `collection` | string | Auth collection slug |
| `email` | string? | The submitted login identifier, when the strategy was reached via a password-style login (gRPC `Login` / the admin form). `nil` for header/token flows (OAuth callback, per-request resolution). |
| `password` | string? | The submitted plaintext password — for strategies that verify credentials against an external system (LDAP, a remote API). `nil` for header/token flows. **Sensitive:** only your strategy receives it; never log it. |
| `remote_addr` | string? | The client's IP address, when known (set on the gRPC login path). |

## CRUD Access

Strategy functions have full CRUD access (via the same TxContext pattern as hooks). They can query the database to look up users.

## Execution Order

For every request (admin or gRPC):

1. The evaluator walks every registered auth collection's `methods` in declaration order.
2. For each method, it checks: (a) the surface filter includes the current request's surface, and (b) the activation discriminator matches (for `strategy`: the named header is present, or `always = true`).
3. The first method that matches and produces a principal wins.
4. If no method matches, the request is anonymous.

Each strategy is bound to its own activation signal — cross-collection accidental authentication is structurally impossible.

## Disabling Password Login

Omit `password_login` from the methods list:

```lua
auth = {
    enabled = true,
    methods = {
        { type = "strategy", name = "sso",
          authenticate = "hooks.auth.sso_check",
          activates_on = { header = "x-sso-assertion" },
          surfaces = { "admin" } },
    },
}
```

When `password_login` is absent:
- The login form shows a message instead of email/password inputs.
- Only the listed strategy methods can authenticate users.
- The `Login` gRPC RPC for this collection returns `INVALID_ARGUMENT`.

Omit `bearer` similarly to refuse JWT authentication (rarely useful — usually paired with omitting `password_login` since the JWT can't be issued without it).

## Performance + safety notes

- **No per-call timeout.** Strategy hooks run synchronously inside a
  spawn-blocking pool. mlua doesn't expose a clean interruption API,
  and abandoning the worker would leak the blocking thread. A hostile
  or buggy strategy can hang the request indefinitely. Keep strategies
  fast (a DB lookup or two, no network calls), and prefer
  header-discriminated activation over `always = true` so slow code
  only runs when the activating header is present.
- **Strategy returns are sanity-checked.** The evaluator refuses any
  returned document with an empty `id` (would silently break session-
  version lookups downstream) and re-runs `is_locked` / `verify_email`
  against the returned doc (so a strategy can't authenticate a locked
  or unverified user even if the strategy code overlooks the check).
- **No session-version on strategy auth.** Bearer / cookie paths
  reject a JWT whose `session_version` doesn't match the user's
  current version; strategies don't issue a JWT to the client (the
  Claims object is internal-only), so there's nothing to compare
  against. Lock the user (`crap-cms user lock -e ...`) to revoke
  strategy access — `is_locked` is the cross-method kill switch.

## Auth Callbacks (OAuth2 / OIDC)

For redirect-based auth flows (OAuth2, OIDC, SAML), use the built-in callback routes:

```
GET/POST /admin/auth/callback/{name}                 # single auth collection
GET/POST /admin/auth/callback/{collection}/{name}    # explicit auth collection
```

Both dispatch to a Lua hook `auth_callback.{name}` which receives request headers and query parameters; the hook returns a user document to create a session. The two routes differ only in how the **target auth collection** is chosen (see *Collection binding* below).

The file lives at `{config_dir}/auth_callback/{name}.lua` (resolved by
`require("auth_callback.{name}")`) and **returns the handler function
directly** — the callback ref `auth_callback.{name}` is the module itself, not
a `module.function` pair.

```lua
-- auth_callback/google.lua
return function(ctx)
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
    local users = crap.collections.find("users", { where = { email = userinfo.email } })
    if #users.documents > 0 then return users.documents[1] end

    return crap.collections.create("users", {
        email = userinfo.email,
        name = userinfo.name,
    })
end
```

> **Verification & lock still apply.** The callback enforces the same account
> guards as password login: a locked account, or an unverified account in a
> collection that requires email verification, is refused a session (the user
> is redirected to login). Return a user your provider has actually
> authenticated.
>
> **Collection binding.** The callback binds the session to one auth collection,
> and the hook-returned user must exist in it. The session can never bind to a
> *different* auth collection by a hook-returned id — that would be a privilege
> escalation across collections.
>
> - The un-scoped route `/admin/auth/callback/{name}` binds to the auth
>   collection **only when there is exactly one**. With two or more auth
>   collections the target is ambiguous, so it fails closed (redirect to login) —
>   use the scoped route instead.
> - The scoped route `/admin/auth/callback/{collection}/{name}` binds to the auth
>   collection named in the URL. Use it when you have multiple auth collections
>   (e.g. `admins` and `customers`): register a distinct provider redirect URI per
>   collection.

To initiate the OAuth flow, add a link on your login page pointing to the provider's authorize URL with your `redirect_uri` set to the matching callback route — `/admin/auth/callback/google` for a single auth collection, or `/admin/auth/callback/customers/google` to bind to the `customers` collection.

## Email MFA

Auth collections can require a second factor after password verification:

```lua
auth = {
    mfa = "email",  -- "email" or false (default)
}
```

When enabled, after successful password/strategy authentication, a 6-digit code is emailed to the user. They must enter the code to complete login. Codes expire after 5 minutes and are single-use.
