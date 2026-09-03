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

For every request (admin or gRPC) the evaluator applies a **fixed precedence** — not the order methods are declared:

1. **Bearer JWT** (`Authorization: Bearer …` / gRPC metadata) — accepted only if the *issuing* collection (named in the claims) lists `bearer` for the current surface.
2. **Session cookie** (`crap_session`, admin only) — accepted only if the issuing collection lists `session_cookie` for the surface.
3. **Always-active strategies** (`activates_on = { always = true }`) whose `surfaces` include the current surface.
4. **Header-activated strategies** — looked up by the request's header names; only strategies whose `activates_on.header` is present on the request run.

The first credential that authenticates wins. Within one collection, strategies run in declaration order; across collections the order is unspecified (`crap-cms status` warns about strategies that could collide). If no path produces a principal, the request is anonymous.

Two consequences worth knowing:

- A credential that **decodes but is invalid** (bad signature, expired, stale `session_version` after a password change, locked or deleted user, unknown collection) **short-circuits** the evaluation — the request is rejected (gRPC `UNAUTHENTICATED`; admin clears the cookie and redirects to login) and the remaining steps never run. A broken explicit credential is surfaced, not silently bypassed.
- A credential that is valid but **not accepted** by any method (its collection dropped `bearer`/`session_cookie` for this surface, and no strategy fired) is also rejected rather than treated as anonymous, so a stale cookie cannot loop the browser.

The **login path** (admin form POST, gRPC `Login`) is separate: the submitted email/password go to `password_login` first, then to each strategy whose `surfaces` include the login surface **and** whose `activates_on` matches the request (an `always` strategy, or one whose header is present). A strategy scoped to `surfaces = {"grpc"}` never runs on the admin form.

Each strategy is bound to its own activation signal — cross-collection accidental authentication is structurally impossible.

**Side effects are transactional.** The `authenticate` function runs inside a transaction that commits only when it returns a user; on `nil` or an error every write it made rolls back. See [Transaction Access](../hooks/transaction-access.md) for the rationale and the designated homes for failed-attempt bookkeeping.

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
- The `Login` gRPC RPC returns `PERMISSION_DENIED` ("Local login is disabled") when the collection has **no strategies either**. With strategies present, `Login` still offers the submitted credentials to every strategy whose `surfaces` include `grpc` and whose `activates_on` matches the request — so a header-activated API-key strategy is unreachable from `Login` unless the header is sent, while an `always` strategy on `grpc` can implement its own credential check.

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

    -- Trust the email ONLY if the provider verified the user owns it. Without
    -- this, a provider (or misconfigured IdP) that returns an unverified address
    -- lets an attacker match — and take over — an existing local user by email.
    -- Google's v2 userinfo uses `verified_email`; OIDC uses `email_verified`.
    if not userinfo.verified_email then return nil end

    -- Find or create user
    local users = crap.collections.find("users", { where = { email = userinfo.email } })
    if #users.documents > 0 then return users.documents[1] end

    return crap.collections.create("users", {
        email = userinfo.email,
        name = userinfo.name,
    })
end
```

> **Trust the provider's email claim before matching by email.** Match (or
> create) a local user by email *only after* confirming the provider **verified**
> that the user owns it — `verified_email` (Google v2 userinfo) or `email_verified`
> (OIDC), as the example does. Skipping this is an account-takeover hole: a
> provider that returns an unverified address lets an attacker match any existing
> local user with that email. This applies to **every** flow — it is not a
> browser-only concern. (The framework's own verify-email gate below checks
> whether the *local* user document is verified; it cannot vouch for the *IdP's*
> claim about the address — only `verified_email` can.)
>
> **Protect browser redirect flows against login-CSRF with `state` (+ PKCE).** A
> browser authorization-code flow should generate a random `state` before
> redirecting to the provider, persist it bound to that browser (an HttpOnly
> cookie), and reject the callback unless the returned `state` matches. Without
> it, an attacker can hand a victim a callback URL carrying the *attacker's*
> `code` and silently log the victim's browser into the attacker's account. The
> hook can compare the two sides itself: `ctx.headers["_query_state"]` is the
> returned value and `ctx.headers["cookie"]` carries the request cookies. PKCE
> additionally hardens the code exchange.
>
> **Non-browser flows don't use a cookie — pick the defense that fits the
> client.** A cookie-bound `state` is a *browser-only* mechanism:
>
> - **Server-to-server (client-credentials)** integrations never hit this
>   redirect callback at all — the backend gets a token from the provider
>   directly and authenticates to the CMS via its API using a custom strategy
>   (the [Authenticate Function](#authenticate-function) at the top of this page).
>   There is no browser and nothing to CSRF.
> - **Native / mobile / CLI** authorization-code flows *do* redirect but share no
>   cookie jar with the CMS, so they rely on **PKCE** (plus a client-held
>   `state`) rather than a cookie.
>
> Only the browser case can use a cookie; the `verified_email` check above,
> by contrast, is required regardless of flow.
>
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

Auth collections can require a second factor after password verification. MFA is a property of the `password_login` method:

```lua
auth = {
    methods = {
        { type = "password_login", mfa = "email" },  -- "email", "custom", or omit for none
        { type = "bearer" },
        { type = "session_cookie" },
    },
}
```

When enabled, after successful password/strategy authentication, a 6-digit code is emailed to the user. They must enter the code to complete login. Codes expire after 5 minutes and are single-use. On the admin UI the code is entered on the MFA page; over gRPC, `Login` returns `mfa_required = true` plus an `mfa_challenge` token and the `VerifyMfa` RPC completes the login (see [gRPC Authentication](../grpc-api/authentication.md#email-mfa)).

**Throttling.** Code *guesses* are limited per user and per IP on both surfaces (the admin MFA page and gRPC `VerifyMfa`), independently of the login limiter — knowing the password does not reset the guess budget. Code *issuance* is throttled on the admin login only: a user who re-submits the login form while over the forgot-password budget is not sent a new code — the previously issued (still valid) code is reused. gRPC `Login` has no issuance throttle beyond the login rate limits, since every `Login` call already costs an Argon2 verification.

### Custom delivery (`mfa = "custom"`)

With `mfa = "custom"`, the CMS still generates, stores, and verifies the
6-digit code — but delivery is yours: the required `mfa_deliver` hook
receives the code and sends it via any channel (SMS, push, chat, …).
Verification is identical to email MFA on both surfaces (admin MFA page /
gRPC `VerifyMfa`).

```lua
auth = {
    methods = {
        { type = "password_login", mfa = "custom", mfa_deliver = "hooks.mfa.send" },
        -- ...
    },
}

-- hooks/mfa.lua
function M.send(ctx)
    -- ctx.collection, ctx.user (field data), ctx.code (6 digits, SENSITIVE —
    -- never log it), ctx.expires_in (seconds). Nested CRUD is available,
    -- e.g. to enqueue the send in a jobs collection.
    my_sms.send(ctx.user.phone, "Your code: " .. ctx.code)
end
```

`mfa = "custom"` without `mfa_deliver` (or the hook without the mode) is a
**startup error** — the pairing is validated so a login can never silently
receive no code. Delivery is best-effort like the built-in email: hook errors
are logged server-side and the previously issued code stays valid.

An optional `mfa_when` hook decides *whether* a verified login needs the second factor — per surface or per user:

```lua
auth = {
    methods = {
        { type = "password_login", mfa = "email", mfa_when = "hooks.auth.mfa_when" },
        -- ...
    },
}

-- hooks/auth.lua
function M.mfa_when(ctx)
    -- ctx.collection, ctx.user (field data), ctx.surface ("admin"/"grpc"),
    -- ctx.headers. Return false/nil to skip MFA for this login,
    -- anything truthy to require it. A hook error fails closed.
    return ctx.user.mfa_enabled == true
end
```
