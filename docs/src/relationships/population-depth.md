# Population Depth

The `depth` parameter controls how deeply relationship fields are populated with full document objects.

## Depth Values

| Depth | Behavior |
|-------|----------|
| `0` | IDs only. Has-one = string ID. Has-many = array of string IDs. |
| `1` | Populate immediate relationships. Replace IDs with full document objects. |
| `2+` | Recursively populate relationships within populated documents. |

## Defaults

Every read surface resolves an omitted `depth` the same way: it falls back
to `[depth] default_depth` from `crap.toml` (default: `1`), clamped to
`max_depth`.

| Operation | Default Depth |
|-----------|--------------|
| `Find` (gRPC) | `depth.default_depth` (default: `1`) |
| `FindByID` (gRPC) | `depth.default_depth` (default: `1`) |
| `crap.collections.find()` (Lua) | `depth.default_depth` (default: `1`) |
| `crap.collections.find_by_id()` (Lua) | `depth.default_depth` (default: `1`) |
| MCP `find_*` / `find_by_id_*` | `depth.default_depth` (default: `1`) |
| `GetGlobal` (gRPC) | `depth.default_depth` (default: `1`) |
| `crap.globals.get()` (Lua) | `depth.default_depth` (default: `1`) |
| MCP `global_read_*` | `depth.default_depth` (default: `1`) |

A global read populates its relationship and upload fields exactly as a
collection read populates a document's (the admin edit form reads with
`depth = 0` and labels its references itself).

To make list endpoints return bare IDs, either pass `depth = 0` explicitly
or set `[depth] default_depth = 0` project-wide.

## Configuration

### Global Config

```toml
[depth]
default_depth = 1   # Fallback when a request omits `depth` (all read surfaces)
max_depth = 10      # Hard cap for all requests (default: 10)
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
local result = crap.collections.posts.find({ depth = 1 })

-- FindByID with depth
local post = crap.collections.posts.find_by_id(id, { depth = 2 })

-- A global read with depth
local site = crap.globals.site_settings.get({ depth = 1 })
```

## Circular Reference Protection

Population tracks the `(collection, id)` pairs on the current **path** — the
documents between the one being populated and the top of the read. A
reference back to a document on that path is kept as a plain ID string
instead of being populated again; the same document reached through a
different branch (say a post's `author` and its `editor` are the same user) is
populated in each branch.

This prevents infinite loops when collections reference each other (e.g.,
posts → users → posts), and it gives every read surface the same shape: a
`Find` and a `FindByID` of the same document return the same tree at every
depth.

## What a Populated Document Contains

A populated document reads exactly as a direct read of it by the same reader
would:

- **Access** — the target collection's `read` access decides whether it is
  shown at all (a hidden has-one target is `null`, a hidden has-many entry is
  dropped); its fields' `access.read` rules and `hidden` flags are applied to
  it.
- **Drafts** — with `draft = true`, a target whose latest version is a
  pending draft shows that draft when the reader has the **target's** draft
  access (bounded by its draft rule's row constraint); otherwise its published
  content. Without `draft = true` a draft-only target is never embedded.
- **Read hooks** — the target collection's `before_read` hooks run once per
  target collection per read (with the reader's user and locale); one that
  aborts hides that collection's populated documents, as a denied read does.
  Its field and collection `after_read` hooks then run on each populated
  document, with the embedding read's operation (`find` / `find_by_id`, or
  `get` when a global read embeds it) — at every depth, fail-open like every
  `after_read`.
- **`collection`** — every populated document carries a `collection` key
  naming its collection (it tells polymorphic targets apart). The name
  `collection` is reserved and cannot be a field name.

## Performance

Population adds queries beyond the main find/find_by_id. How many depends on the depth and number of relationship fields.

### How It Works

- **Batch fetching:** `Find` with `depth >= 1` collects all referenced IDs across all returned documents per relationship field and fetches them in a single `IN (...)` query. This means one extra query per relationship field, regardless of how many documents reference it.
- **Recursive batching:** At `depth >= 2`, the same batch strategy applies recursively — populated documents' relationships are batch-fetched at each depth level.
- **Per-document fetching:** `FindByID` populates a single document.
- **Join fields:** a `Find` looks a join field up for every returned document
  in one query, keeping at most the join's `limit` per document (default 10).

### Query Cost

| Scenario | Extra Queries |
|----------|--------------|
| `depth=0` | 0 |
| `depth=1`, `Find` returning N docs, M relationship fields | M queries (one batch per field) |
| `depth=1`, `FindByID`, M relationship fields | M queries |
| `depth=2`, `Find`, M fields at level 1, K fields at level 2 | M + (M × K) queries |

Join fields add one query per join field at each depth level (`Find`), or
per document (`FindByID`). A document referenced from several places is
fetched once per level but populated in each place it appears.

### Cache Backend

A pluggable cache backend avoids redundant population queries across requests. The default `memory` backend caches populated documents in-process using DashMap. For multi-server deployments, use `redis` for a shared cache. The cache is automatically cleared on any write operation (create, update, delete).

```toml
[cache]
backend = "memory"      # "memory" (default), "redis", "none", "custom"
max_entries = 10000      # Soft cap for memory backend
max_age_secs = 60        # Optional: periodic full cache clear (default: 0 = off)
```

**Backends:**
- `"memory"` — In-process DashMap with soft entry cap. Default. Good for single-server.
- `"redis"` — Shared Redis cache. Requires `--features redis`. Good for multi-server.
- `"none"` — Cache disabled. Each request creates its own temporary cache (no cross-request sharing).
- `"custom"` — Lua-delegated (planned, not yet implemented).

**Trade-off:** If the database is modified outside the API (e.g., direct SQL, external tools), cached data can become stale. Set `max_age_secs` to limit staleness, or use `backend = "none"` to disable.

### Recommendations

- **Pass `depth=0` explicitly on hot list endpoints** (or set
  `default_depth = 0` project-wide) when the list view doesn't render related
  data — the out-of-the-box default populates one level on every read.
- **Use `select` to limit populated fields.** Non-selected relationship fields are skipped entirely during population.
- **Set per-field `max_depth`** on relationship fields that don't need deep population.
- **If you need related data in a list**, use `depth=1` with `select` to populate only the specific relationship fields you need.
