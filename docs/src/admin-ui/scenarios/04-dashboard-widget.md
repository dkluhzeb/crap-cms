# Scenario 4: Dashboard widget that fetches a 3rd-party API

**Goal**: add a card to the admin dashboard that displays current
weather (or any external API value) for ops awareness.

**Difficulty**: medium. Two files: a slot template + a
`crap.template_data` registration in `init.lua`.

**You'll touch**: `templates/slots/dashboard_widgets/<name>.hbs`,
`init.lua`.

## Approach

The dashboard's `dashboard/index.hbs` declares a `dashboard_widgets`
slot:

```hbs
{{slot "dashboard_widgets"}}
```

Drop a `.hbs` file into the slot directory and it renders alongside
any other contributions. To pull in dynamic data, register a
template-data function via `crap.template_data.register` and call
it from your slot template with `{{data "name"}}`. The function
runs **on demand** — only when the slot template references it, so
pages that don't show the widget pay no cost.

This is the same pattern the [custom page scenario](05-custom-page.md)
uses for live counts.

## Step 1 — drop the slot file

```hbs
{{!-- <config_dir>/templates/slots/dashboard_widgets/weather.hbs --}}
<div class="card">
  <div class="card__header">
    <span class="material-symbols-outlined">cloud</span>
    <h3>Weather</h3>
  </div>
  <div class="card__body">
    {{#with (data "weather_now")}}
      <p class="metric">{{temp}}°C, {{condition}}</p>
      <p class="muted">{{location}} — updated {{updated_at}}</p>
    {{else}}
      <p class="muted">Weather data unavailable.</p>
    {{/with}}
  </div>
</div>
```

The filename inside `slots/dashboard_widgets/` doesn't matter for
routing — anything `.hbs` renders. The filename only controls
**render order** (alphabetical), so prefix with `NN-` if you need
to control where your card appears among other widgets.

## Step 2 — register the template-data function

Add to `<config_dir>/init.lua`:

```lua
crap.template_data.register("weather_now", function(ctx)
  -- ctx is the page render context (read-only here, just for context).
  -- Return a table; the template binds it via {{#with (data "weather_now")}}.

  local api_key = crap.env.get("CRAP_WEATHER_API_KEY")
  if not api_key then
    crap.log.warn("weather_now: CRAP_WEATHER_API_KEY not set")
    return nil  -- {{#with}} {{else}} branch handles missing data
  end

  local response = crap.http.request({
    method = "GET",
    url = "https://api.weather.example/current?location=berlin",
    headers = { ["Authorization"] = "Bearer " .. api_key },
    timeout = 5000,  -- ms
  })

  if response.status ~= 200 then
    crap.log.warn(string.format(
      "weather_now: API returned status %d", response.status
    ))
    return nil
  end

  local data = crap.json.decode(response.body)

  return {
    temp = data.temperature,
    condition = data.condition,
    location = data.location,
    updated_at = os.date("%H:%M"),
  }
end)
```

Notes on the registration:

- **`crap.template_data.register("name", fn)`** — `name` is what
  the template uses in `{{data "name"}}`. The fn runs on demand.
- **`fn(ctx)`** — the page render context (`ctx.user`, `ctx.nav`,
  `ctx.crap.site_name`, etc.). Read-only by convention; this isn't
  the place to mutate page-wide state.
- **Return a table** — `{{#with (data "weather_now")}}` binds it.
  Returning `nil` makes `{{#with}}` fall to its `{{else}}` branch.
- **`crap.env.get(key)`** — read-only env access **restricted to
  `CRAP_*` and `LUA_*` prefixed vars** (returns `nil` for any
  other key). So your env var must be named `CRAP_WEATHER_API_KEY`,
  not `WEATHER_API_KEY`.
- **`crap.http.request(opts)`** — single options table with
  `method`, `url`, `headers`, `body`, `timeout`. Returns a table
  with `status`, `headers`, `body`. SSRF protections are on by
  default (private-IP requests blocked unless explicitly allowed).
- **`crap.json.decode(s)`** — parse a JSON string into a Lua table.
- **`crap.log.warn(msg)`** / `crap.log.info(msg)` — structured
  logging. Output appears in the crap-cms server log alongside Rust
  log entries.

## Step 3 — restart

Lua loads at startup. Restart crap-cms to pick up your new
`init.lua` registration:

```
$ pkill -f 'crap-cms serve' && cargo run -- --config /path/to/config serve
```

Templates reload per request in `dev_mode = true`, but Lua
registrations are evaluated once at startup. There's no
file-watcher.

## Step 4 — verify

Open `/admin`. The card renders alongside any other dashboard
widgets:

```
┌──────────────────────────────────────┐
│ Welcome back, alice@example.com       │
└──────────────────────────────────────┘

┌─────────────────┐  ┌──────────────────┐
│ Weather         │  │ Recent activity  │
│ 18°C, Cloudy    │  │ ...              │
│ Berlin — 14:32  │  └──────────────────┘
└─────────────────┘
```

If the API is down or the env var is missing, the `{{else}}`
branch renders `Weather data unavailable.` instead — no error
page.

## Caching the API response

`crap.template_data` doesn't ship a built-in cache. The function
runs on each render. For a 3rd-party API you don't want to hit on
every request, two options:

**Option A — module-level Lua cache.** Cache the result in a Lua
upvalue with a TTL:

```lua
do
  local cached_value, cached_at = nil, 0
  local TTL_SECONDS = 600   -- 10 minutes

  crap.template_data.register("weather_now", function(ctx)
    local now = os.time()
    if cached_value and (now - cached_at) < TTL_SECONDS then
      return cached_value
    end

    -- ... fetch as in Step 2 ...
    local fresh = { temp = ..., condition = ..., ... }

    cached_value = fresh
    cached_at = now
    return fresh
  end)
end
```

This works well for low-traffic admin pages. The cache is per-Lua-VM
(crap-cms uses a small VM pool), so the API is hit once per VM per
TTL window.

**Option B — push the cache to the API call.** Some upstream APIs
support cache headers; if the source is your own service, put a
cache layer in front of it (Varnish, Redis, CloudFront). Then your
template-data function calls a fast cached endpoint and
`crap.template_data` doesn't need to cache itself.

For most admin-dashboard widgets, Option A is overkill — admin
traffic is low. Skip caching unless you measure a problem.

## What this scenario *doesn't* cover

- **Auto-refresh in the browser** — the page renders server-side.
  For a card that updates without a full reload, use HTMX:

  ```hbs
  <div hx-get="/admin/p/widgets/weather" hx-trigger="every 600s" hx-swap="innerHTML">
    {{#with (data "weather_now")}}...{{/with}}
  </div>
  ```

  This requires a separate custom page at `/admin/p/widgets/weather`
  that returns just the inner HTML. See [Scenario 5](05-custom-page.md).
- **Per-user widgets** — the slot renders for everyone. Filter
  inside the slot template using `{{user.role}}` if you want
  role-gated widgets.

## Verifying

```
$ crap-cms templates status
  · templates/slots/dashboard_widgets/weather.hbs  —  user-original (no upstream counterpart)
  · init.lua                                        —  user-original (no upstream counterpart)
```

Slot files are user-original by definition (no upstream counterpart
since they're additive). They never drift; `templates status` just
confirms they exist.
