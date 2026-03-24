# crap.util

Utility functions available everywhere the `crap` global is accessible.

## crap.util.slugify(str)

Generate a URL-safe slug from a string. Lowercases, replaces non-alphanumeric characters with hyphens, and collapses consecutive hyphens.

```lua
crap.util.slugify("Hello World")      -- "hello-world"
crap.util.slugify("Hello, World!")     -- "hello-world"
crap.util.slugify("  multiple   spaces  ") -- "multiple-spaces"
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `str` | string | Input string |
| **Returns** | string | URL-safe slug |

## crap.util.nanoid()

Generate a unique nanoid string (21 characters by default).

```lua
local id = crap.util.nanoid()
-- e.g., "V1StGXR8_Z5jdHi6B-myT"
```

| **Returns** | string | Random nanoid |

## crap.util.json_encode(value)

Encode a Lua value (table, string, number, boolean, nil) as a JSON string.

> **Tip:** `crap.json.encode()` and `crap.json.decode()` are aliases — see [crap.json](json.md).

```lua
local json = crap.util.json_encode({ name = "test", count = 42 })
-- '{"count":42,"name":"test"}'
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `value` | any | Lua value to encode |
| **Returns** | string | JSON string |

## crap.util.json_decode(str)

Decode a JSON string into a Lua value.

```lua
local data = crap.util.json_decode('{"name":"test","count":42}')
print(data.name)   -- "test"
print(data.count)  -- 42
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `str` | string | JSON string |
| **Returns** | any | Decoded Lua value |

## Common Hook Patterns

### Auto-Slug Generation

```lua
function M.auto_slug(ctx)
    if not ctx.data.slug or ctx.data.slug == "" then
        ctx.data.slug = crap.util.slugify(ctx.data.title or "")
    end
    return ctx
end
```

### Generate Unique Identifiers

```lua
function M.set_ref_code(ctx)
    if ctx.operation == "create" then
        ctx.data.ref_code = "REF-" .. crap.util.nanoid()
    end
    return ctx
end
```

### Serialize Complex Data

```lua
function M.store_metadata(ctx)
    if type(ctx.data.metadata) == "table" then
        ctx.data.metadata = crap.util.json_encode(ctx.data.metadata)
    end
    return ctx
end
```

## Table Helpers

### crap.util.deep_merge(a, b)

Deep merge two tables. `b` overwrites `a`. Returns a new table.

```lua
local merged = crap.util.deep_merge(
    { name = "old", nested = { x = 1 } },
    { name = "new", nested = { y = 2 } }
)
-- { name = "new", nested = { x = 1, y = 2 } }
```

### crap.util.pick(tbl, keys)

Return a table with only the listed keys.

```lua
local picked = crap.util.pick({ a = 1, b = 2, c = 3 }, { "a", "c" })
-- { a = 1, c = 3 }
```

### crap.util.omit(tbl, keys)

Return a table without the listed keys.

```lua
local result = crap.util.omit({ a = 1, b = 2, c = 3 }, { "b" })
-- { a = 1, c = 3 }
```

### crap.util.keys(tbl) / crap.util.values(tbl)

Extract keys or values as arrays.

```lua
local k = crap.util.keys({ a = 1, b = 2 })   -- { "a", "b" }
local v = crap.util.values({ a = 1, b = 2 })  -- { 1, 2 }
```

### crap.util.map(tbl, fn) / crap.util.filter(tbl, fn) / crap.util.find(tbl, fn)

Functional array operations.

```lua
local doubled = crap.util.map({ 1, 2, 3 }, function(v) return v * 2 end)
-- { 2, 4, 6 }

local evens = crap.util.filter({ 1, 2, 3, 4 }, function(v) return v % 2 == 0 end)
-- { 2, 4 }

local found = crap.util.find({ 1, 2, 3 }, function(v) return v > 1 end)
-- 2
```

### crap.util.includes(tbl, value)

Check if an array contains a value.

```lua
crap.util.includes({ "a", "b", "c" }, "b")  -- true
```

### crap.util.is_empty(tbl)

Check if a table has no entries.

```lua
crap.util.is_empty({})       -- true
crap.util.is_empty({ a = 1 }) -- false
```

### crap.util.clone(tbl)

Shallow copy a table.

```lua
local original = { a = 1 }
local copy = crap.util.clone(original)
copy.a = 2
print(original.a)  -- 1 (unchanged)
```

## String Helpers

### crap.util.trim(str)

Strip leading and trailing whitespace.

```lua
crap.util.trim("  hello  ")  -- "hello"
```

### crap.util.split(str, sep)

Split a string by separator. Returns an array.

```lua
crap.util.split("a,b,c", ",")  -- { "a", "b", "c" }
```

### crap.util.starts_with(str, prefix) / crap.util.ends_with(str, suffix)

```lua
crap.util.starts_with("hello world", "hello")  -- true
crap.util.ends_with("hello world", "world")    -- true
```

### crap.util.truncate(str, max_len, suffix?)

Truncate a string with optional suffix (default: `"..."`).

```lua
crap.util.truncate("Hello, World!", 8)         -- "Hello..."
crap.util.truncate("Hello, World!", 8, " >>")  -- "Hello >>"
```

## Date Helpers

### crap.util.date_now()

Get current UTC time as ISO 8601 string.

```lua
local now = crap.util.date_now()  -- "2024-01-15T10:30:00+00:00"
```

### crap.util.date_timestamp()

Get current Unix timestamp in seconds.

```lua
local ts = crap.util.date_timestamp()  -- 1705312200
```

### crap.util.date_parse(str)

Parse a date string to Unix timestamp. Tries RFC 3339, then `%Y-%m-%d %H:%M:%S`, then `%Y-%m-%d`.

```lua
local ts = crap.util.date_parse("2024-01-15T10:30:00Z")
local ts2 = crap.util.date_parse("2024-01-15")
```

### crap.util.date_format(timestamp, format)

Format a Unix timestamp using chrono format syntax.

```lua
local str = crap.util.date_format(1705312200, "%Y-%m-%d")  -- "2024-01-15"
```

### crap.util.date_add(timestamp, seconds) / crap.util.date_diff(a, b)

Arithmetic on timestamps.

```lua
local tomorrow = crap.util.date_add(crap.util.date_timestamp(), 86400)
local diff = crap.util.date_diff(tomorrow, crap.util.date_timestamp())  -- 86400
```
