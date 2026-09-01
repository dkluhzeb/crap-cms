# Auth Methods

Each auth collection declares an **ordered list of methods** that say how a request can prove identity for that collection. The four method types compose to express anything from "standard password user" to "machine-to-machine with API keys only".

```lua
crap.collections.define("users", {
    auth = {
        enabled = true,
        methods = crap.auth.default_methods(),  -- password_login + bearer + session_cookie
    },
})
```

## The four method types

| Type | Purpose | Default surfaces | Notes |
|---|---|---|---|
| `password_login` | Enables the `Login` RPC; issues JWTs. Owns the password-only knobs (`mfa`, `mfa_when`, `mfa_deliver`, `verify_email`, `forgot_password`). | n/a (Login is one RPC) | At most one per collection. |
| `bearer` | Accept JWTs in `Authorization: Bearer …` (HTTP) / gRPC metadata. | `{"grpc", "admin"}` | At most one per collection. |
| `session_cookie` | Accept the `crap_session` cookie. | `{"admin"}` | Admin-HTTP only in practice. |
| `strategy` | Custom Lua authenticator (API keys, SSO headers, mTLS). Declares its own `activates_on` discriminator. | `{"admin"}` | Any number per collection. |

## Surface scoping

Every non-`password_login` method takes a `surfaces` array listing the host transports it applies to. Today's surfaces are `"admin"` and `"grpc"`. A method whose `surfaces` doesn't include the current request's surface is skipped.

This lets you express:

```lua
-- Bearer JWT works on both surfaces, cookie only on admin:
{ type = "bearer",         surfaces = { "grpc", "admin" } },
{ type = "session_cookie", surfaces = { "admin" } },

-- API-key strategy only for gRPC clients:
{ type = "strategy",
  name = "svc-key",
  authenticate = "hooks.auth.svc_key",
  activates_on = { header = "x-service-key" },
  surfaces = { "grpc" } },
```

## Activation discriminators (`strategy` only)

A `strategy` method MUST declare when it fires. Two forms:

### `activates_on = { header = "x-api-key" }`

The strategy is invoked only when the named header (lowercase) is present on the request. Strategy returns nil if it can't authenticate; the evaluator moves on.

This is the safe, common case. Each strategy is bound to its own header, so two collections both listing API-key strategies on different headers cannot accidentally authenticate each other's principals.

### `activates_on = { always = true }`

The strategy is invoked on every request that passes the surface filter. The strategy itself decides per-request whether to authenticate. This is the escape hatch for:

- **mTLS / TLS client cert** — the credential isn't a header, it's on the connection.
- **Multi-signal strategies** — strategy looks at several headers + a query param and composes.
- **External IdP introspection** — strategy sends the request to an IdP and lets it decide.

Always-active strategies emit a startup warning since accidental always-on patterns run on every request. Use a `header` discriminator unless you specifically need the catch-all.

`activates_on = { always = false }` is rejected at config-load time — it has no useful meaning. To disable a method, remove it from `methods`.

## Default and helper methods

`crap.auth.default_methods()` returns the standard set:

```lua
{
    { type = "password_login" },
    { type = "bearer",         surfaces = { "grpc", "admin" } },
    { type = "session_cookie", surfaces = { "admin" } },
}
```

`crap.auth.with_defaults(extras)` returns `default_methods()` with the `extras` list appended — the usual shape for "standard auth plus my strategy":

```lua
auth = {
    enabled = true,
    methods = crap.auth.with_defaults({
        { type = "strategy",
          name = "api-key",
          authenticate = "hooks.auth.api_key",
          activates_on = { header = "x-api-key" },
          surfaces = { "grpc" } },
    }),
}
```

## Evaluation order

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

Bearer JWTs unambiguously identify their issuing collection (claims include the collection slug), so JWTs from collection A never authenticate as collection B even if both have `bearer` in their methods.

## Common patterns

### Password user collection
```lua
auth = {
    enabled = true,
    methods = crap.auth.default_methods(),
}
```

### Password user + API key for gRPC clients
```lua
auth = {
    enabled = true,
    methods = crap.auth.with_defaults({
        { type = "strategy", name = "api-key",
          authenticate = "hooks.auth.api_key",
          activates_on = { header = "x-api-key" },
          surfaces = { "grpc" } },
    }),
}
```

### Machine-to-machine — API key only, no password login, no JWT
```lua
auth = {
    enabled = true,
    methods = {
        { type = "strategy", name = "svc-key",
          authenticate = "hooks.auth.svc_key",
          activates_on = { header = "x-service-key" },
          surfaces = { "grpc" } },
    },
}
```
With no `password_login`, the `Login` RPC for this collection returns `INVALID_ARGUMENT`. With no `bearer`, JWTs aren't accepted (and aren't issued — there's no `password_login` to issue them). The API key is the only path in.

### Admin-only-with-MFA, gRPC clients use Bearer
```lua
auth = {
    enabled = true,
    methods = {
        { type = "password_login", mfa = "email", verify_email = true },
        { type = "bearer",         surfaces = { "grpc", "admin" } },
        { type = "session_cookie", surfaces = { "admin" } },
    },
}
```

gRPC password logins on this collection get the MFA challenge too (`Login`
returns `mfa_challenge`, completed via `VerifyMfa`). To require MFA on one
surface only — or per user — add an `mfa_when` gate:

```lua
{ type = "password_login", mfa = "email", mfa_when = "hooks.auth.mfa_when" },

-- hooks/auth.lua: MFA for admin logins only
function M.mfa_when(ctx)
    return ctx.surface == "admin"
end
```

## Validation rules (enforced at startup)

Load errors (the definition file is rejected):

- An explicit empty `methods = {}` (omit the key to get the defaults instead).
- An unknown key on `auth` or on any method, or an unknown method `type`.
- A `strategy` without a non-empty `authenticate` hook ref.
- A `strategy` without `activates_on` (`{ header = "x-..." }` or `{ always = true }`).

Startup errors (boot fails):

- More than one `password_login` or more than one `bearer` on one collection.
- Any method with an empty `surfaces` list — it could never fire.
- `activates_on = { header = "" }` — no request carries an empty header name.
- `mfa = "custom"` without `mfa_deliver` (or `mfa_deliver` without `mfa = "custom"`).
- A hook ref (`authenticate`, `mfa_when`, `mfa_deliver`) that does not resolve.

Startup warnings (logged, boot continues):

- Any `strategy` with `activates_on = { always = true }` — it runs on every request that reaches its surfaces.
- Multiple always-active strategies on the same surface, or multiple header-activated strategies bound to the same `(header, surface)` — the winner is unpredictable across collections.
- A collection with `enabled = true` and no `password_login`, whose only login path is therefore a strategy.

## See also

- [Custom Strategies](custom-strategies.md) — the Lua function contract, `ctx.headers`, returning a user document.
- [Auth Collections](auth-collections.md) — the collection-level schema concerns.
- [Login Flow](login-flow.md) — what the `Login` RPC actually does, MFA, password verification.
