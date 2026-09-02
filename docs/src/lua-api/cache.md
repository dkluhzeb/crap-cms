# crap.cache

Register a custom cross-request cache backend, used when `crap.toml` sets
`[cache] backend = "custom"`. The registered handler backs the populate
cache described in [Cache Internals](../internals/cache.md): the CMS calls
it for every cache operation on the gRPC/admin/MCP read surfaces and for
the write-through `clear` after every write.

Use it to plug in a shared store the built-in backends don't cover — an
HTTP KV service (Cloudflare KV, Upstash), a company-internal cache tier,
or an instrumented wrapper. For single-server setups prefer `memory`, and
for multi-server setups with Redis available prefer `redis` — both avoid
the per-operation Lua round-trip.

## crap.cache.register(handler)

**Init-only** — call it from `init.lua` (or a file it requires); the boot
fails when `backend = "custom"` is configured and no handler was
registered. The handler table is strict: unknown keys are load errors.

| Key | Required | Contract |
|-----|----------|----------|
| `get(key)` | yes | Return the stored value (string) or `nil` on miss. Raise an error only for a real failure — it propagates to the caller like a Redis error would. |
| `set(key, value)` | yes | Store `value` byte-exact. Values are opaque binary strings (serialized documents) — never transform them. |
| `delete(key)` | yes | Remove one key; no error when absent. |
| `clear()` | yes | Remove **all** entries. Called after every write on any surface, so in a cluster it must clear for every node. |
| `has(key)` | no | Return whether the key exists. Falls back to a `get` probe when omitted. |

```lua
-- init.lua
crap.cache.register({
  get = function(key)
    local resp = crap.http.request({
      url = "https://kv.example.com/cache/" .. crap.util.slugify(key),
      headers = { Authorization = "Bearer " .. crap.env.get("KV_TOKEN") },
    })
    if resp.status == 404 then return nil end
    return resp.body
  end,
  set = function(key, value)
    crap.http.request({
      method = "PUT",
      url = "https://kv.example.com/cache/" .. crap.util.slugify(key),
      headers = { Authorization = "Bearer " .. crap.env.get("KV_TOKEN") },
      body = value,
    })
  end,
  delete = function(key)
    crap.http.request({
      method = "DELETE",
      url = "https://kv.example.com/cache/" .. crap.util.slugify(key),
      headers = { Authorization = "Bearer " .. crap.env.get("KV_TOKEN") },
    })
  end,
  clear = function()
    crap.http.request({
      method = "POST",
      url = "https://kv.example.com/cache/flush",
      headers = { Authorization = "Bearer " .. crap.env.get("KV_TOKEN") },
    })
  end,
})
```

> **The handler must be stateless per VM.** It runs in the hook runner's
> pooled Lua VMs — `init.lua` executes once *per VM*, and a different VM
> may serve each operation. A handler that stashed values in a Lua table
> would `set` into one VM and miss from another (and write-through clears
> from hooks run on yet another). Always delegate to a **shared external
> store**; use `crap.env` for credentials.

## Semantics

- **Keys** are opaque strings (e.g. `populate:<collection>:<id>:…`);
  **values** are opaque binary strings. Store and return them byte-exact.
- **Errors propagate.** A raised error in `get`/`set`/`clear` surfaces to
  the caller exactly like a Redis failure — it is never silently treated
  as a miss.
- **TTL is yours.** `max_entries`, `max_age_secs`, `redis_url` and
  `prefix` do not apply to the custom backend; give entries a TTL in the
  external store if you want expiry beyond the write-through clears.
- **Performance.** Every operation checks a Lua VM out of the hook
  runner's pool (growing the pool under contention) and then runs the
  handler. The populate cache exists to avoid database work, so a handler
  that is slower than the database defeats the point — measure, and
  include a cached-read scenario in any load test.
