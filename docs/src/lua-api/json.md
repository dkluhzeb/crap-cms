# crap.json

JSON encode/decode functions.

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

Two boundary cases follow from Lua having one table type and one number
type, and apply to every Lua↔JSON crossing (hook arguments, CRUD data,
`crap.json`): an **empty table encodes as `{}`**, so a JSON `[]` that is
decoded and re-encoded comes back as `{}`; and an **integer above
`i64::MAX`** (2⁶³−1) that Lua cannot hold exactly arrives as a float.

### Null

`crap.null` encodes as `null`; a `nil`-valued key is simply absent (Lua
cannot store `nil` in a table). Use it to keep an explicit null:

```lua
crap.json.encode({ a = crap.null, list = { 1, crap.null } })
-- '{"a":null,"list":[1,null]}'
```

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

A `null` **object field** decodes to `nil` (the key is absent); a `null`
**array element** decodes to `crap.null`, so the array keeps its length:

```lua
local list = crap.json.decode('[1, null, 3]')
print(#list)                -- 3
print(list[2] == crap.null) -- true (crap.null is truthy — compare, don't test)
local obj = crap.json.decode('{"x": null}')
print(obj.x)                -- nil
```

See [Null values](overview.md#null-values).

## Notes

- **Integer precision** — JSON integers that fit a 64-bit signed integer decode to exact Lua integers; larger integers and all fractional numbers decode to Lua floats (`f64`), which are exact only up to 2^53 (~9 × 10^15). If you need to preserve very large IDs exactly, encode them as strings.
- **Nesting depth** — encoder rejects tables nested more than 64 levels deep to guard against runaway recursion. A self-referential Lua table (`t.a = t`) will exceed this limit and error rather than looping forever.
- **Decode of untrusted input** — decoding enforces serde_json's recursion limit (128 nesting levels): deeper input errors instead of overflowing the stack. Size is not limited — cap attacker-controlled payload sizes upstream (e.g. via `[hooks] http_max_response_bytes` for fetched bodies).

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
