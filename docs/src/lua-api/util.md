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
