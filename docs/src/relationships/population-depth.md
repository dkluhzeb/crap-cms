# Population Depth

The `depth` parameter controls how deeply relationship fields are populated with full document objects.

## Depth Values

| Depth | Behavior |
|-------|----------|
| `0` | IDs only. Has-one = string ID. Has-many = array of string IDs. |
| `1` | Populate immediate relationships. Replace IDs with full document objects. |
| `2+` | Recursively populate relationships within populated documents. |

## Defaults

| Operation | Default Depth |
|-----------|--------------|
| `Find` (gRPC) | `0` (avoids N+1 on list queries) |
| `FindByID` (gRPC) | `depth.default_depth` from `crap.toml` (default: `1`) |
| `crap.collections.find()` (Lua) | `0` |
| `crap.collections.find_by_id()` (Lua) | `0` |

## Configuration

### Global Config

```toml
[depth]
default_depth = 1   # Default for FindByID (default: 1)
max_depth = 10       # Hard cap for all requests (default: 10)
```

### Per-Field Max Depth

Cap the depth for a specific relationship field, regardless of the request-level depth:

```lua
{
    name = "author",
    type = "relationship",
    relationship = {
        collection = "users",
        max_depth = 1,  -- never populate deeper than 1, even if depth=5
    },
}
```

## Usage

### gRPC

```bash
# Find with depth=1
grpcurl -plaintext -d '{
    "collection": "posts",
    "depth": 1
}' localhost:50051 crap.ContentAPI/Find

# FindByID with depth=2
grpcurl -plaintext -d '{
    "collection": "posts",
    "id": "abc123",
    "depth": 2
}' localhost:50051 crap.ContentAPI/FindByID
```

### Lua API

```lua
-- Find with depth
local result = crap.collections.find("posts", { depth = 1 })

-- FindByID with depth
local post = crap.collections.find_by_id("posts", id, { depth = 2 })
```

## Circular Reference Protection

The population algorithm tracks visited `(collection, id)` pairs. If a document has already been visited in the current recursion path, it's kept as a plain ID string instead of being populated again.

This prevents infinite loops when collections reference each other (e.g., posts → users → posts).

## Performance Considerations

- `depth=0` requires no extra queries
- `depth=1` requires one query per relationship field per document
- Higher depths multiply this exponentially
- Use `max_depth` on fields to limit expensive deep populations
- `Find` defaults to `depth=0` to avoid N+1 issues on list endpoints
