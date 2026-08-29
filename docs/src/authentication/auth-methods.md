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
| `password_login` | Enables the `Login` RPC; issues JWTs. Owns the password-only knobs (`mfa`, `mfa_when`, `verify_email`, `forgot_password`). | n/a (Login is one RPC) | At most one per collection. |
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

For each incoming request:

1. The evaluator walks every auth collection's `methods` in declaration order.
2. For each method, it checks (a) the surface filter matches and (b) the activation discriminator matches the request.
3. The first method that matches and produces a principal wins; the request proceeds authenticated as that principal.
4. If no method matches, the request is anonymous.

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

- `enabled = true` with `methods = {}` → **error**.
- More than one `password_login` → **error**.
- More than one `bearer` → **error**.
- `strategy` without `activates_on` → **error** (auto-defaults to `always = true` in the parser, but the startup warning fires).
- Any `strategy` with `activates_on = { always = true }` → **warning** in the logs.
- Multiple always-active strategies on the same surface → **louder warning** (auth depends on registration order).

## See also

- [Custom Strategies](custom-strategies.md) — the Lua function contract, `ctx.headers`, returning a user document.
- [Auth Collections](auth-collections.md) — the collection-level schema concerns.
- [Login Flow](login-flow.md) — what the `Login` RPC actually does, MFA, password verification.
