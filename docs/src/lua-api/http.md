# crap.http

Outbound HTTP client for making requests from Lua hooks and init.lua.

## Functions

### `crap.http.request(opts)`

Make a blocking HTTP request.

**Parameters:**
- `opts` (table):
  - `url` (string, required) — Request URL.
  - `method` (string, optional) — HTTP method. Default: `"GET"`. Supported: `GET`, `POST`, `PUT`, `PATCH`, `DELETE`, `HEAD`.
  - `headers` (table, optional) — Request headers as key-value pairs.
  - `body` (string, optional) — Request body.
  - `timeout` (integer, optional) — Timeout in seconds. Default: `30`.

**Returns:** table — Response with fields:
- `status` (integer) — HTTP status code.
- `headers` (table) — Response headers as key-value pairs.
- `body` (string) — Response body as a string.

**Errors:** Throws a Lua error on transport failures (DNS, connection refused, timeout).

```lua
-- Simple GET
local resp = crap.http.request({ url = "https://api.example.com/data" })
if resp.status == 200 then
    local data = crap.util.json_decode(resp.body)
    crap.log.info("Got " .. #data .. " items")
end

-- POST with JSON body
local resp = crap.http.request({
    url = "https://api.example.com/webhook",
    method = "POST",
    headers = {
        ["Content-Type"] = "application/json",
        ["Authorization"] = "Bearer " .. crap.env.get("CRAP_API_TOKEN"),
    },
    body = crap.util.json_encode({ event = "document.created", id = ctx.data.id }),
    timeout = 10,
})
```

## Notes

- Uses [ureq](https://docs.rs/ureq) (blocking HTTP client). Since Lua hooks run inside `spawn_blocking`, blocking I/O is correct and won't stall the async runtime.
- Non-2xx responses are **not** errors — they return normally with the status code. Only transport-level failures (DNS, timeout, connection refused) throw Lua errors.
- Available in both init.lua and hooks.

## Security

### Private network blocking

When `hooks.allow_private_networks` is `false` (the default), `crap.http.request` resolves the URL hostname and rejects requests targeting loopback, private (RFC 1918), link-local, and unspecified IP addresses. This prevents SSRF attacks against internal services. Set `allow_private_networks = true` in `crap.toml` only if your hooks need to reach internal services.

### DNS rebinding limitation

The private network check resolves the hostname **before** the HTTP request is made, but the underlying HTTP client (ureq) performs its own DNS resolution when connecting. A malicious DNS server could return a public IP during the validation check and then a private IP during the actual connection (DNS rebinding). This is a known limitation — if this is a concern in your environment, use `allow_private_networks = false` (the default) and additionally restrict outbound network access at the firewall/network level.
