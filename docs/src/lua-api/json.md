# crap.json

JSON encode/decode functions. These are the same functions available as `crap.util.json_encode` / `crap.util.json_decode`, exposed under a dedicated namespace for convenience.

## crap.json.encode(value)

Encode a Lua value (table, string, number, boolean, nil) as a JSON string.

```lua
local json = crap.json.encode({ name = "test", count = 42 })
-- '{"count":42,"name":"test"}'
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `value` | any | Lua value to encode |
| **Returns** | string | JSON string |

## crap.json.decode(str)

Decode a JSON string into a Lua value.

```lua
local data = crap.json.decode('{"name":"test","count":42}')
print(data.name)   -- "test"
print(data.count)  -- 42
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `str` | string | JSON string |
| **Returns** | any | Decoded Lua value |

## Notes

- **Integer precision** — JSON numbers are decoded into Lua number (`f64` under the hood). Integers larger than 2^53 (~9 × 10^15) lose precision. If you need to preserve large IDs exactly, encode them as strings before serializing.
- **Nesting depth** — encoder rejects tables nested more than 64 levels deep to guard against runaway recursion. A self-referential Lua table (`t.a = t`) will exceed this limit and error rather than looping forever.
- **Decode of untrusted input** — decoding is permissive and does not currently enforce a maximum depth on the input side. Keep that in mind when processing large attacker-controlled payloads.

## Common Patterns

### Webhook Payload

```lua
crap.http.request({
    method = "POST",
    url = webhook_url,
    headers = { ["Content-Type"] = "application/json" },
    body = crap.json.encode({
        event = "new_inquiry",
        name = inquiry.name,
        email = inquiry.email,
    }),
})
```

### Parse API Response

```lua
local resp = crap.http.request({ url = "https://api.example.com/data" })
if resp.status == 200 then
    local data = crap.json.decode(resp.body)
    crap.log.info("Got " .. #data .. " items")
end
```
