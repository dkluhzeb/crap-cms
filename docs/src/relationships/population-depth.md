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
crap.fields.relationship({
    name = "author",
    relationship = {
        collection = "users",
        max_depth = 1,  -- never populate deeper than 1, even if depth=5
    },
})
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

## Performance

Population adds queries beyond the main find/find_by_id. How many depends on the depth and number of relationship fields.

### How It Works

- **Batch fetching:** `Find` with `depth >= 1` collects all referenced IDs across all returned documents per relationship field and fetches them in a single `IN (...)` query. This means one extra query per relationship field, regardless of how many documents reference it.
- **Recursive batching:** At `depth >= 2`, the same batch strategy applies recursively — populated documents' relationships are batch-fetched at each depth level.
- **Per-document fetching:** `FindByID` populates a single document. Join fields (reverse lookups) also use per-document queries since they require a `WHERE` clause per parent.

### Query Cost

| Scenario | Extra Queries |
|----------|--------------|
| `depth=0` | 0 |
| `depth=1`, `Find` returning N docs, M relationship fields | M queries (one batch per field) |
| `depth=1`, `FindByID`, M relationship fields | M queries |
| `depth=2`, `Find`, M fields at level 1, K fields at level 2 | M + (M × K) queries |

Join fields add one query per document per join field at each depth level.

### Populate Cache

For high-traffic deployments, an opt-in cross-request cache avoids redundant population queries. When enabled, populated documents are cached in memory and reused across requests. The cache is automatically cleared on any write operation (create, update, delete).

```toml
[depth]
populate_cache = true              # Enable cross-request populate cache (default: false)
populate_cache_max_age_secs = 60   # Optional: periodic full cache clear (default: 0 = off)
```

**When to enable:** High read traffic with repeated deep population of the same related documents (e.g., many posts referencing the same set of authors/categories).

**Trade-off:** If the database is modified outside the API (e.g., direct SQL, external tools), cached data can become stale. Set `populate_cache_max_age_secs` to limit staleness, or leave the cache disabled (default).

### Recommendations

- **Use `depth=0` for list endpoints.** `Find` defaults to `depth=0` for this reason. Fetch related data when displaying a single document instead.
- **Use `select` to limit populated fields.** Non-selected relationship fields are skipped entirely during population.
- **Set per-field `max_depth`** on relationship fields that don't need deep population.
- **If you need related data in a list**, use `depth=1` with `select` to populate only the specific relationship fields you need.
