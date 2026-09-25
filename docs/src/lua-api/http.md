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
  - `body` (string, optional) — Request body. Any Lua string, binary data included — the bytes are sent unchanged.
  - `timeout` (number, optional) — Timeout in seconds; fractional values allowed (`0.5` = 500 ms). Must be positive. Default: `30`. Applies to each request of a redirect chain. Inside a job handler (or a queued custom-provider email) each request is also bounded by the time left before the job's `timeout`: a request still running at that deadline is stopped and raises the job-timeout error.

  Unknown keys in `opts` are a hard error (a typo like `timout` can't
  silently fall back to the default).

**Returns:** table — Response with fields:
- `status` (integer) — HTTP status code.
- `headers` (table) — Response headers as key-value pairs.
- `body` (string) — Response body as a string holding the bytes as received; binary responses (images, archives) come back intact.

**Errors:** Throws a Lua error on transport failures (DNS, connection refused, timeout).

```lua
-- Simple GET
local resp = crap.http.request({ url = "https://api.example.com/data" })
if resp.status == 200 then
    local data = crap.json.decode(resp.body)
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
    body = crap.json.encode({ event = "document.created", id = ctx.data.id }),
    timeout = 10,
})
```

## Notes

- Uses [reqwest](https://docs.rs/reqwest) (blocking HTTP client). Since Lua hooks run inside `spawn_blocking`, blocking I/O is correct and won't stall the async runtime.
- Non-2xx responses are **not** errors — they return normally with the status code. Only transport-level failures (DNS, timeout, connection refused) throw Lua errors.
- **Redirects** follow standard method/body semantics: `307`/`308` preserve the method and body; `303` (and non-GET/HEAD `301`/`302`) switch to `GET` and drop the body. Credential headers (`Authorization`, `Cookie`, …) are only replayed to the original origin — a redirect to another scheme, host **or port** does not receive them. Up to 10 redirects are followed. Only `301`, `302`, `303`, `307` and `308` are followed; any other `3xx` (e.g. `304 Not Modified` answering an `If-None-Match` request) is returned to the caller as the response.
- **Response size**: a body larger than `[hooks] http_max_response_bytes` is a hard error, not a silent truncation. Duplicate response headers (e.g. multiple `Set-Cookie`) are comma-joined in `resp.headers`.
- Available in both init.lua and hooks.
- **TLS certificate verification** is always enabled (reqwest's default with the `rustls-tls` feature). There is no opt-out — `crap.http.request` will not connect to servers with invalid or self-signed certificates. Use a proper CA-signed certificate on any HTTPS endpoint you call.

## Security

### Private network blocking

When `hooks.allow_private_networks` is `false` (the default), `crap.http.request` resolves the URL hostname and rejects requests targeting loopback, private (RFC 1918), link-local, unspecified, "this network" (`0.0.0.0/8`), CGNAT (`100.64.0.0/10`), IETF-assignment (`192.0.0.0/24`) and benchmarking (`198.18.0.0/15`) IPv4 addresses, and unique-local (`fc00::/7`), link-local (`fe80::/10`) and site-local (`fec0::/10`) IPv6 addresses. IPv6 forms that carry an IPv4 target — IPv4-mapped (`::ffff:a.b.c.d`), IPv4-compatible (`::a.b.c.d`), NAT64 (`64:ff9b::/96`) and 6to4 (`2002::/16`) — are judged by the embedded IPv4 address; the local-use NAT64 prefix `64:ff9b:1::/48` is refused as a whole. This prevents SSRF attacks against internal services. Set `allow_private_networks = true` in `crap.toml` only if your hooks need to reach internal services.

### DNS rebinding protection

DNS is resolved once during validation, checked against the SSRF policy, and the validated IP is pinned via `reqwest::ClientBuilder::resolve()`. The HTTP client connects to the exact validated address — no second DNS lookup occurs. Redirects are individually resolved, validated, and pinned before following.

For the same reason the client ignores `HTTP_PROXY`, `HTTPS_PROXY` and `ALL_PROXY` from the process environment: a proxy resolves the hostname itself, which would bypass both the private-network check and the pin. `crap.http` therefore always connects directly; an egress proxy cannot be put in front of it.
