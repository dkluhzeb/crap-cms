# crap.collections

Collection definition and runtime CRUD operations.

## crap.collections.define(slug, config)

Define a new collection. Call this in collection definition files (`collections/*.lua`).

```lua
crap.collections.define("posts", {
    labels = { singular = "Post", plural = "Posts" },
    fields = {
        { name = "title", type = "text", required = true },
    },
})
```

See [Collection Definition Schema](../collections/definition-schema.md) for all config options.

## crap.collections.find(collection, query?)

Find documents matching a query. Returns a result table with `documents` and `total`.

**Only available inside hooks with transaction context.**

```lua
local result = crap.collections.find("posts", {
    filters = {
        status = "published",
        title = { contains = "hello" },
    },
    order_by = "-created_at",
    limit = 10,
    offset = 0,
    depth = 1,
})

-- result.documents = array of document tables
-- result.total = total count (before limit/offset)

for _, doc in ipairs(result.documents) do
    print(doc.id, doc.title)
end
```

### Query Parameters

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `filters` | table | `{}` | Field filters. See [Filter Operators](filter-operators.md). |
| `order_by` | string | `nil` | Sort field. Prefix with `-` for descending. |
| `limit` | integer | `nil` | Max results to return. |
| `offset` | integer | `nil` | Number of results to skip. |
| `depth` | integer | `0` | Population depth for relationship fields. |

## crap.collections.find_by_id(collection, id, opts?)

Find a single document by ID. Returns the document table or `nil`.

**Only available inside hooks with transaction context.**

```lua
local doc = crap.collections.find_by_id("posts", "abc123")
if doc then
    print(doc.title)
end

-- With population depth
local doc = crap.collections.find_by_id("posts", "abc123", { depth = 2 })
```

### Options

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `depth` | integer | `0` | Population depth for relationship fields. |

## crap.collections.create(collection, data)

Create a new document. Returns the created document.

**Only available inside hooks with transaction context.**

```lua
local doc = crap.collections.create("posts", {
    title = "New Post",
    slug = "new-post",
    status = "draft",
})
print(doc.id)  -- auto-generated nanoid
```

## crap.collections.update(collection, id, data)

Update an existing document. Returns the updated document.

**Only available inside hooks with transaction context.**

```lua
local doc = crap.collections.update("posts", "abc123", {
    title = "Updated Title",
    status = "published",
})
```

## crap.collections.delete(collection, id)

Delete a document. Returns `true` on success.

**Only available inside hooks with transaction context.**

```lua
crap.collections.delete("posts", "abc123")
```
