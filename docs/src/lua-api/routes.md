# crap.routes

`crap.routes` mounts **custom HTTP endpoints** on the admin server, handled
entirely in Lua. Where [custom pages](../admin-ui/scenarios/05-custom-page.md)
render an admin-gated HTML template, custom routes are arbitrary HTTP endpoints —
webhooks, JSON APIs, OAuth initiation, health probes, server-rendered fragments —
that return a structured response.

Handlers run in **pool-mode**, exactly like [job handlers](jobs.md): each
`crap.collections.*` call autocommits its own transaction, and
`crap.transaction(fn)` wraps a block in a single atomic transaction. The full
`crap.*` API is available inside a handler.

Scaffold one with `crap-cms make route <name> --method GET --path /x`, which
writes `routes/<name>.lua` (a typed handler) and prints the matching
`crap.routes.register(...)` snippet. `crap.routes.list()` returns every
registered route as a `{ path, methods, access }` table (`crap.RouteInfo`).

## Registering a route

Call `crap.routes.register` from `init.lua` (or any definition file). The handler
is a [hook ref](hooks.md) resolving to `<config_dir>/routes/<name>.lua`, which
returns `function(ctx) ... end`. Wrap it in `crap.any.route_handler(fn)` so the
editor types `ctx` as `crap.RouteContext` (an identity pass-through at runtime).

```lua
-- init.lua
crap.routes.register({
  path    = "/webhooks/stripe",      -- literal path; Axum `{param}` syntax for params
  method  = "POST",                   -- or { "GET", "POST" }
  handler = "routes.stripe_webhook",  -- → routes/stripe_webhook.lua
  access  = false,                    -- optional (see Access below)
  rate_limit = { max = 60, window = 60 }, -- optional, per-IP
  csrf    = false,                    -- optional (default false)
  max_body = 65536,                   -- optional, bytes (overrides routes.max_body)
  options = { foo = "bar" },          -- optional, surfaced as ctx.options
})
```

| Key | Required | Meaning |
|-----|----------|---------|
| `path` | yes | Mount path (after the optional `routes.prefix`). `{param}` segments become `ctx.params`. May not collide with a reserved prefix (`/admin`, `/static`, `/api`, `/uploads`, `/mcp`, `/health`, `/ready`, `/`). |
| `method` | yes | A method or array of methods. One of `GET/HEAD/POST/PUT/PATCH/DELETE/OPTIONS`. |
| `handler` | yes | Hook ref to the handler function. |
| `access` | no | `omitted`/nil = **public**; `false` = registered but **disabled** (404); a hook ref = **gated** (evaluated before the handler; falsey → 403). `access = true` is **rejected** — it would silently mean "public", a footgun for a caller expecting "require auth"; omit `access` for a public route or pass a hook ref to gate it. |
| `rate_limit` | no | `{ max, window }` (window in seconds), per-IP. |
| `csrf` | no | `true` enforces the admin double-submit CSRF token on mutating methods. Default `false` (custom routes are API-style). |
| `max_body` | no | Per-route request body-size limit in bytes. Overrides the `routes.max_body` config default. |
| `options` | no | Arbitrary table surfaced to the handler as `ctx.options`. |

Bad methods, reserved/duplicate paths, and unresolvable `handler`/`access` refs
**fail at startup**, not at first request.

## Configuration (`crap.toml`)

```toml
[routes]
prefix   = ""       # URL prefix for every custom route (default: none → literal path)
max_body = "1MB"    # default request body-size limit (per-route `max_body` overrides)
```

By default routes mount at their literal `path`; set e.g.
`routes.prefix = "/ext"` to namespace them all. The prefix must not push routes
under a reserved built-in prefix (`/api`, `/admin`, `/static`, `/uploads`,
`/mcp`, `/health`, `/ready`) — that's rejected at startup. Each route's body is
capped at its `max_body` (or `routes.max_body` when unset) — independent of the
upload size limit.

Rate limiting uses the configured `auth.rate_limit_backend`, so with a Redis
backend a route's `rate_limit` is shared across server instances (not per-process).

## The handler

```lua
-- routes/stripe_webhook.lua
return function(ctx)
  if ctx.json == nil then
    return { status = 400, json = { error = "expected JSON" } }
  end
  crap.collections.create("events", { kind = ctx.json.type })
  return { json = { ok = true } }
end
```

### `ctx`

| Field | Type | Notes |
|-------|------|-------|
| `ctx.method` | string | Upper-cased. |
| `ctx.path` | string | The matched route path. |
| `ctx.params` | table | Path params (`/items/{id}` → `ctx.params.id`). |
| `ctx.query` | table | Parsed query string. |
| `ctx.headers` | table | Lowercase header names. |
| `ctx.cookies` | table | Pre-parsed request cookies. |
| `ctx.body` | string? | Raw request body. |
| `ctx.json` | table? | Parsed body when `Content-Type` is JSON. |
| `ctx.form` | table? | Parsed body for `application/x-www-form-urlencoded`. |
| `ctx.user` | doc? | Authenticated user, or `nil` (anonymous). |
| `ctx.collection` | string? | The user's auth collection. |
| `ctx.ip` | string | Client IP (honors `trusted_proxies`). |
| `ctx.options` | table? | The route's `options`. |

Notes:
- `ctx.body` is decoded as UTF-8 (lossy for non-UTF-8 input); raw binary bodies
  aren't exposed. JSON/text webhook payloads are UTF-8, so signature checks over
  `ctx.body` are byte-accurate.
- `ctx.query` / `ctx.cookies` (and a form body) are flat maps — a repeated key
  keeps the **last** value. Response `headers` are likewise single-valued (a
  repeated name keeps the last); use `cookies` for multiple `Set-Cookie`s.
- The `access` gate and the handler run in **separate** pool-mode steps with no
  shared transaction or snapshot, so re-check any security-critical invariant
  inside the handler (a concurrent write can change a row between the two).
- A `redirect` requires a 3xx `status` (default 302); a non-3xx `status` with
  `redirect` is rejected. `SameSite=None` cookies are sent with `Secure` forced
  on. Invalid cookie names/values (spaces, `;`, control chars) are rejected.

### The response

Return one of:

- a **table** envelope: `{ status?, headers?, cookies?, body?, json?, redirect? }`,
- a bare **string** → `200 text/plain`,
- **nil** / **false** → `404`.

```lua
return {
  status   = 200,                       -- default 200 (302 if `redirect` set)
  json     = { ok = true },             -- serialized; sets application/json
  body     = "raw text",                -- use one of body / json
  headers  = { ["x-custom"] = "1" },
  redirect = "https://example.com/next",
  cookies  = {
    { name = "sid", value = "…", http_only = true, secure = true,
      same_site = "lax", path = "/", max_age = 600 },
  },
}
```

## Access

Custom routes are **public by default** — a route is a good way to serve a public
endpoint (a webhook, a server-rendered fragment). To protect one, set `access` to
a hook ref; it receives the same `ctx` and returns truthy to allow:

```lua
crap.routes.register({ path = "/admin-only", method = "GET",
  handler = "routes.secret", access = "access.is_admin" })

-- access/is_admin.lua
return function(ctx)
  return ctx.user ~= nil and ctx.user.role == "admin"
end
```

`ctx.user` is resolved from the session cookie or `Bearer` token (the same
evaluator the admin UI uses), so anonymous requests see `ctx.user == nil`.

## Transactions

A handler is pool-mode: individual `crap.collections.*` writes autocommit. For
multi-step atomicity wrap them in `crap.transaction`:

```lua
return function(ctx)
  crap.transaction(function()
    local order = crap.collections.create("orders", ctx.json)
    crap.collections.update("inventory", order.item_id, { reserved = true })
  end)
  return { json = { ok = true } }
end
```

## Example: secure browser OAuth `state`

A custom route can set an `HttpOnly` cookie before redirecting to a provider —
the missing piece for a CSRF-safe browser OAuth flow (see
[Custom Strategies](../authentication/custom-strategies.md)):

```lua
-- routes/oauth_start.lua  (registered public)
return function(ctx)
  local state = crap.util.nanoid()
  return {
    redirect = "https://accounts.google.com/o/oauth2/v2/auth?…&state=" .. state,
    cookies  = { { name = "oauth_state", value = state, http_only = true,
                   secure = true, same_site = "lax", path = "/auth", max_age = 600 } },
  }
end
```

The `auth_callback` hook then verifies `ctx.headers["cookie"]` against the
returned `state`.
