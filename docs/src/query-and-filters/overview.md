# Query & Filters

Unified reference for querying documents across both the Lua API and gRPC API.

> **Note on soft-deleted rows**: collections with `soft_delete = true` automatically exclude rows where `_deleted_at IS NOT NULL` from `Find`, `Count`, and populate. Filters on system columns (names starting with `_`) are rejected. Use the request-level `trash = true` / `draft = true` flags to reach internally-scoped data.

## Filter Operators

| Operator | Lua | gRPC (where) | SQL |
|----------|-----|-------------|-----|
| Equals | `status = "published"` or `{ equals = "val" }` | `{"equals": "val"}` | `field = ?` |
| Not equals | `{ not_equals = "val" }` | `{"not_equals": "val"}` | `field != ?` |
| Like | `{ like = "pattern%" }` | `{"like": "pattern%"}` | `field LIKE ? ESCAPE '\'` |
| Contains | `{ contains = "text" }` | `{"contains": "text"}` | `field LIKE '%text%' ESCAPE '\'` (wildcards `%` and `_` in the search text are escaped) |
| Greater than | `{ greater_than = "10" }` | `{"greater_than": "10"}` | `field > ?` |
| Less than | `{ less_than = "10" }` | `{"less_than": "10"}` | `field < ?` |
| Greater/equal | `{ greater_than_or_equal = "10" }` | `{"greater_than_or_equal": "10"}` | `field >= ?` |
| Less/equal | `{ less_than_or_equal = "10" }` | `{"less_than_or_equal": "10"}` | `field <= ?` |
| In | `{ ["in"] = { "a", "b" } }` | `{"in": ["a", "b"]}` | `field IN (?, ?)` |
| Not in | `{ not_in = { "a", "b" } }` | `{"not_in": ["a", "b"]}` | `field NOT IN (?, ?)` |
| Exists | `{ exists = true }` | `{"exists": true}` | `field IS NOT NULL` |
| Not exists | `{ not_exists = true }` | `{"not_exists": true}` | `field IS NULL` |

> **Has-many fields** (lists) are matched element by element — `equals` means "some element equals", `not_equals` "no element equals". See [Has-many fields](#has-many-fields-element-by-element).
>
> **Note:** `exists`/`not_exists` accept only the boolean `true`. `{ exists = false }` (or any non-boolean value) is rejected with an error on every surface — it is never silently dropped or read as IS NOT NULL. Use `not_exists = true` for IS NULL.
>
> **Ranges:** one operator object may carry several operators, ANDed together — `{ greater_than_or_equal = "2024-01-01", less_than = "2025-01-01" }`.
>
> **`in`/`not_in` element rules:** elements must be scalars — a nested array/object element is a **hard error** (never silently dropped); mixed scalar types are coerced to their string forms. An empty `in = {}` matches **nothing**; an empty `not_in = {}` matches **everything** — take care when the list is built dynamically (an accidentally-empty `not_in` combined with a bulk delete selects every document). An empty **group** inside `or` (`{"or": [{}]}`) is a hard **error** for the same reason: one vacuous alternative would make the whole `or` match every row.
>
> **Scalar shorthand works on every surface:** bare values like `{ count = 42 }` or `{ active = true }` (Lua) and `{"count": 42}` / `{"active": true}` (gRPC/MCP `where` JSON) are coerced to a string `equals` — numbers via their decimal form, booleans as `"true"`/`"false"`. All surfaces share one filter grammar, so shorthand, operator objects, and `or` groups behave identically everywhere.

### Matching semantics

These are consistent across both backends (SQLite and Postgres) and across the
admin UI, Lua, and gRPC surfaces — so a filter behaves the same everywhere:

- **`like` / `contains` are case-insensitive** (for ASCII). `{ like = "john%" }`
  matches `"John Doe"`. (SQLite `LIKE` is ASCII-case-insensitive by default;
  Postgres uses `ILIKE` to match.) In a `like` pattern, `%` matches any run of
  characters (line breaks included) and `_` matches one character; write `\%`,
  `\_` or `\\` to match a literal `%`, `_` or backslash. A pattern that ends in
  a lone backslash is rejected. `contains` escapes any `%`/`_` in your text, so
  they match literally.
- **Ordering (`greater_than`/`less_than`) follows the field's type.** Filter
  values are coerced to the column type from the field definition: a `number`
  field compares **numerically** (`"100" > "50"` is true), a `text` field
  compares **lexicographically** (`"100" < "50"`, because `'1' < '5'`), and a
  `date` field compares lexicographically over its normalized ISO form (which
  orders correctly). If you want numeric ordering, use a `number` field — don't
  store numbers in a `text` field.

### Dates: a bare day covers the whole day

A date is stored as a UTC instant (`2026-01-15T09:30:00.000Z`), but a filter
often names a calendar day — the admin filter builder sends `YYYY-MM-DD` for a
`date` field and for `created_at` / `updated_at`. On a `date` field (in a
column, a group, or an array or blocks row) and on the
`created_at` / `updated_at` timestamps, an operand that is exactly a valid
`YYYY-MM-DD` covers that whole **UTC** day, `[D 00:00, D+1 00:00)`:

| Operator | `D` = `2026-01-15` matches |
|---|---|
| `equals` | any instant on the 15th |
| `not_equals` | any instant not on the 15th |
| `greater_than` | from `2026-01-16T00:00:00.000Z` on |
| `greater_than_or_equal` | from `2026-01-15T00:00:00.000Z` on |
| `less_than` | before `2026-01-15T00:00:00.000Z` |
| `less_than_or_equal` | before `2026-01-16T00:00:00.000Z` |
| `in` / `not_in` | on (not on) any listed day; a listed instant stays exact |

An operand with a time (`2026-01-15T09:00`, `2026-01-15T09:00:00+02:00`) keeps
its exact comparison, normalized to the stored UTC form. The day is always the
UTC day of the stored value — a `timezone = true` field included, whose value
is stored in UTC (a bare date written to such a field is the zone's local noon,
which for a zone more than 12 hours from UTC — UTC+13/+14, UTC−12 — lies on the
neighbouring UTC day). To select a local day in another zone, pass the zone's
midnight bounds with an offset:
`greater_than_or_equal = "2026-01-15T00:00:00-05:00"` and
`less_than = "2026-01-16T00:00:00-05:00"`. A NULL date matches no comparison,
`not_equals` and `not_in` a non-empty list included. SQL and the in-memory
evaluator (live events, population gating) read dates alike.

### Has-many fields: element by element

A field that holds a list — a `text`, `number`, `select` or `radio` field with
`has_many = true`, a has-many relationship or upload filtered by `.id`, and a
has-many relationship or upload inside an array or blocks row — is filtered
**element by element**, on every surface and both backends:

| Operator | Matches a document when… |
|----------|--------------------------|
| `equals` | **some** element equals the value |
| `not_equals` | **no** element equals the value |
| `in` | **some** element is in the list |
| `not_in` | **no** element is in the list |
| `like` / `contains` | **some** element matches the pattern / contains the text |
| `greater_than`, `less_than`, `…_or_equal` | **some** element satisfies the comparison |
| `exists` | the list holds at least one element |
| `not_exists` | the list is empty |

Each element compares as the field's single value would: a `number` list
numerically (`{ scores = { greater_than = "9" } }` matches `[10]`), a `text`
list in its stored form, `like`/`contains` case-insensitively. A relationship
list inside a row compares its ids — a polymorphic entry by the id after its
`collection/`, as a top-level list's `.id` does. An empty list and an unset
(NULL) list hold no elements: every positive operator misses them, and
`not_equals`, `not_in` and `not_exists` match them.

Every stored list is a list: when `has_many` is switched on over existing
values (or a list is retyped), the schema sync stores each old value as a list
once — see [Changing a definition that has data](../database/overview.md#changing-a-definition-that-has-data).

Conditions on the same list combine by AND like any others, each judged over
the elements on its own: in the admin list, the rows *tags is a* AND *tags is
b* (`where[tags][equals]=a&where[tags][equals]=b`) match documents tagged both
`a` **and** `b`. To accept any of several values, use `in` (or an OR row in
the admin filter builder).

A has-many field cannot be used as `order_by` — its values have no single
order — and a sort on one is rejected with a validation error.

## Sorting

Prefix a field name with `-` for descending order. A group's sub-field sorts by
either spelling filters accept — `seo.title` or `seo__title` (`-seo.title` for
descending). When `order_by` is omitted, results are sorted by `created_at DESC` (newest first) for collections with timestamps, or `id ASC` otherwise. When sorting by a non-id field, an `id` tiebreaker is always appended for stable ordering.

**Relevance order:** `order_by = "_rank"` (only valid together with `search`) sorts by search relevance, best first — FTS5 `bm25()` on SQLite, `ts_rank` on Postgres — with a stable `id` tiebreaker. It requires page/offset pagination (relevance is not cursor-stable) and skips the drafts `_status` prepend described below: when you search ranked, relevance wins. Without an FTS index yet, it degrades to `id` order, matching the search filter's behavior.

`_status` and `_deleted_at` can be ordered by only where they exist — on a drafts-enabled collection and a soft-delete collection respectively; elsewhere they are a validation error on every surface (the admin list answers 400).

On a **drafts-enabled** collection, `_status ASC` is prepended to every sort (on all surfaces) so draft rows group before published ones in mixed reads; when the read is pinned to a single status (a normal published-only read, or `draft = true`) the prepend is a no-op and your sort applies exactly.

**Lua:**

```lua
crap.collections.posts.find({ order_by = "-created_at" })
```

**gRPC:**

```bash
grpcurl -plaintext -d '{
    "collection": "posts",
    "order_by": "-created_at"
}' localhost:50051 crap.ContentAPI/Find
```

## Pagination

Use `limit` and `page` for pagination. The response includes a nested `pagination` object with total count and page info.

**Lua:**

```lua
local result = crap.collections.posts.find({
    limit = 10,
    page = 3,
})
-- result.pagination.total_docs   = 150 (total matching documents)
-- result.pagination.limit       = 10
-- result.pagination.total_pages  = 15
-- result.pagination.page        = 3   (1-based)
-- result.pagination.page_start   = 21  (1-based index of first doc on this page)
-- result.pagination.has_next_page = true
-- result.pagination.has_prev_page = true
-- result.pagination.prev_page    = 2
-- result.pagination.next_page    = 4
-- #result.documents             = 10  (this page)
```

**gRPC:**

```bash
grpcurl -plaintext -d '{
    "collection": "posts",
    "limit": "10",
    "page": "3"
}' localhost:50051 crap.ContentAPI/Find
```

## Cursor-Based Pagination

Cursor-based pagination is opt-in via `[pagination] mode = "cursor"` in `crap.toml`. When enabled, the `pagination` object includes opaque `start_cursor` and `end_cursor` tokens instead of `page`/`total_pages`. These represent the cursors of the first and last documents in the result set. Pass `after_cursor` (forward) or `before_cursor` (backward) on the next request to navigate from any cursor position.

`after_cursor`/`before_cursor` and an explicit `page` **greater than 1** are mutually exclusive (an error); `page = 1` is the default and is tolerated alongside a cursor. `after_cursor` and `before_cursor` are mutually exclusive with each other. In page mode (`[pagination] mode = "page"`, the default), cursor parameters are **silently ignored** rather than rejected.

**Lua:**

```lua
-- First page
local result = crap.collections.posts.find({
    order_by = "-created_at",
    limit = 10,
})
-- result.pagination.has_next_page  = true
-- result.pagination.has_prev_page  = false
-- result.pagination.start_cursor  = "eyJpZCI6ImFiYzEyMyJ9"  (cursor of first doc)
-- result.pagination.end_cursor    = "eyJpZCI6Inh5ejc4OSJ9"  (cursor of last doc)

-- Next page (forward)
local page2 = crap.collections.posts.find({
    order_by = "-created_at",
    limit = 10,
    after_cursor = result.pagination.end_cursor,
})

-- Previous page (backward)
local page1_again = crap.collections.posts.find({
    order_by = "-created_at",
    limit = 10,
    before_cursor = page2.pagination.start_cursor,
})
```

**gRPC:**

```bash
# First page
grpcurl -plaintext -d '{
    "collection": "posts",
    "order_by": "-created_at",
    "limit": "10"
}' localhost:50051 crap.ContentAPI/Find
# Response pagination includes start_cursor / end_cursor when cursor mode is active

# Next page (forward)
grpcurl -plaintext -d '{
    "collection": "posts",
    "order_by": "-created_at",
    "limit": "10",
    "after_cursor": "eyJpZCI6Inh5ejc4OSJ9"
}' localhost:50051 crap.ContentAPI/Find

# Previous page (backward)
grpcurl -plaintext -d '{
    "collection": "posts",
    "order_by": "-created_at",
    "limit": "10",
    "before_cursor": "eyJpZCI6ImFiYzEyMyJ9"
}' localhost:50051 crap.ContentAPI/Find
```

Cursors encode the position of a document in the sorted result set. They are opaque — do not parse or construct them manually. `start_cursor` and `end_cursor` are always present when the result set is non-empty.

A cursor records the value the sort orders by. For an all-locales read
(`locale = "all"`) sorted by a localized field — top level or inside a group —
documents hold the field as a `{ en = …, de = … }` map, and the sort (and the
cursor) uses the default locale's value.

## Combining Filters

Multiple filters are combined with AND:

**Lua:**

```lua
crap.collections.posts.find({
    where = {
        status = "published",
        created_at = { greater_than = "2024-01-01" },
        title = { contains = "update" },
    },
    order_by = "-created_at",
    limit = 10,
})
```

**gRPC:**

```bash
grpcurl -plaintext -d '{
    "collection": "posts",
    "where": "{\"status\":\"published\",\"created_at\":{\"greater_than\":\"2024-01-01\"},\"title\":{\"contains\":\"update\"}}",
    "order_by": "-created_at",
    "limit": "10"
}' localhost:50051 crap.ContentAPI/Find
```

## OR Filters

Use the `or` key to combine groups of conditions with OR logic. Each element in the `or` array is an object whose fields are AND-ed together. Multiple `or` groups are joined with OR.

**Lua:**

```lua
-- title contains "hello" OR category = "news"
crap.collections.posts.find({
    where = {
        ["or"] = {
            { title = { contains = "hello" } },
            { category = "news" },
        },
    },
})

-- status = "published" AND (title contains "hello" OR title contains "world")
crap.collections.posts.find({
    where = {
        status = "published",
        ["or"] = {
            { title = { contains = "hello" } },
            { title = { contains = "world" } },
        },
    },
})

-- Multi-condition groups: (status = "published" AND title contains "hello") OR (status = "draft")
crap.collections.posts.find({
    where = {
        ["or"] = {
            { status = "published", title = { contains = "hello" } },
            { status = "draft" },
        },
    },
})
```

**gRPC:**

```bash
# title contains "hello" OR category = "news"
grpcurl -plaintext -d '{
    "collection": "posts",
    "where": "{\"or\":[{\"title\":{\"contains\":\"hello\"}},{\"category\":\"news\"}]}"
}' localhost:50051 crap.ContentAPI/Find

# status = "published" AND (title contains "hello" OR title contains "world")
grpcurl -plaintext -d '{
    "collection": "posts",
    "where": "{\"status\":\"published\",\"or\":[{\"title\":{\"contains\":\"hello\"}},{\"title\":{\"contains\":\"world\"}}]}"
}' localhost:50051 crap.ContentAPI/Find
```

Top-level filters and `or` groups are combined with AND. Each object inside the `or` array can have multiple fields which are AND-ed together within that group.

## Field Selection (`select`)

Use `select` to specify which fields to return. Reduces data transfer and skips relationship hydration/population for non-selected fields. The `id`, `created_at`, and `updated_at` fields are always included.

**Lua:**

```lua
-- Return only title and status
crap.collections.posts.find({
    select = { "title", "status" },
})

-- Works with find_by_id too
crap.collections.posts.find_by_id(id, {
    select = { "title", "status" },
})
```

**gRPC:**

```bash
# Return only title and status fields
grpcurl -plaintext -d '{
    "collection": "posts",
    "select": ["title", "status"]
}' localhost:50051 crap.ContentAPI/Find

# FindByID with select
grpcurl -plaintext -d '{
    "collection": "posts",
    "id": "abc123",
    "select": ["title", "status"]
}' localhost:50051 crap.ContentAPI/FindByID
```

**Behavior:**
- `select` is optional. When omitted or empty, all fields are returned (backward compatible).
- `id` is always included. `created_at`, `updated_at` and `_status` are returned **only when named** in `select` (they are still fetched internally so cursor pagination keeps its composite order, but they are stripped from the response).
- A name that matches no top-level field (or `id`/`created_at`/`updated_at`/`_status`) is a **hard error** naming the entry — it used to silently select nothing.
- Selecting a group field name (e.g., `"seo"`) includes all its sub-fields.
- Relationship fields not in `select` are skipped during population (saves N+1 queries).

## Field Validation

All filter field names and `order_by` fields are validated against the collection's field definitions, and so is every dot-notation path down to its last segment: an unknown field, an array or block sub-field the rows do not have, a sub-path into a field that has none, a path ending on a container (a group, a nested array), or one ending on a `join` field (which stores no value) is rejected before any SQL runs. The error is a validation error naming the path (or `order_by`), reported the same way on every surface and for every operation that filters — `Find`, `Count`, `UpdateMany` and `DeleteMany` answer `INVALID_ARGUMENT` over gRPC, MCP reports the message, Lua raises it. This also keeps field names from ever reaching SQL unchecked.

## Draft Parameter (Versioned Collections)

Collections with `versions = { drafts = true }` automatically filter by `_status = 'published'` on `Find` and `FindByID` queries. Use the `draft` parameter to change this behavior.

**Lua:**

```lua
-- Default: only published documents
local published = crap.collections.articles.find({})

-- Include drafts
local all = crap.collections.articles.find({ draft = true })

-- FindByID: get the latest version (may be a draft)
local latest = crap.collections.articles.find_by_id(id, { draft = true })
```

**gRPC:**

```bash
# Default: only published
grpcurl -plaintext -d '{"collection": "articles"}' \
    localhost:50051 crap.ContentAPI/Find

# Include drafts
grpcurl -plaintext -d '{"collection": "articles", "draft": true}' \
    localhost:50051 crap.ContentAPI/Find

# FindByID: get latest version snapshot
grpcurl -plaintext -d '{"collection": "articles", "id": "abc123", "draft": true}' \
    localhost:50051 crap.ContentAPI/FindByID
```

Filters on system columns (field paths starting with `_`, e.g. `_status`, `_deleted_at`) are rejected. Use the `draft = true` request flag to include drafts, and `trash = true` to reach soft-deleted rows — these are the supported entry points.

See [Versions & Drafts](../collections/versions.md) for the full workflow.

## Nested Field Filters (Dot Notation)

You can filter on sub-fields of group, array, blocks, and has-many relationship fields using dot notation.

### Group Fields

Group sub-fields can be filtered using dot notation. Internally, `seo.meta_title` is converted to `seo__meta_title` (the flat column name). The double-underscore syntax also continues to work.

An array, blocks or has-many relationship/upload **inside a group** keeps its
rows in its own join table (`{collection}_{group}__{field}`), and a path
reaches it through the group spelled either way — `seo.links.url` or
`seo__links.url`, `seo.tags.id` — then continues exactly as for a top-level
array, blocks or has-many field (below). Groups nest: `seo.social.links.url`.
Errors name the path as you wrote it.

**Lua:**

```lua
crap.collections.pages.find({
    where = {
        ["seo.meta_title"] = { contains = "SEO" },
    },
})
```

**gRPC:**

```bash
grpcurl -plaintext -d '{
    "collection": "pages",
    "where": "{\"seo.meta_title\":{\"contains\":\"SEO\"}}"
}' localhost:50051 crap.ContentAPI/Find
```

### Array Sub-Fields

Filter by sub-field values in array rows. Uses an `EXISTS` subquery against the array join table. Returns parent documents that have **at least one** array row matching the condition — for every operator, `not_equals` included (`variants.color not_equals "red"` finds a document with at least one non-red variant). A has-many sub-field — a list, or a has-many relationship's ids (`variants.related`) — is then read element by element inside that row. A sub-field inside a layout `row`, `collapsible` or `tabs` is named directly (`variants.width`), since a row stores it under its own name.

A group, nested array or nested blocks sub-field is stored as JSON in its row, and a path continues into it at any depth, exactly as it does inside a block row: `variants.dimensions.width` (group), `variants.sizes.label` (a nested array — some nested row matches), `variants.parts._block_type` (nested blocks). `variants.id` filters by the row's own id — the id every read returns for the row and every write round-trips. A row nested inside another row's JSON has no filterable id, and the row table's bookkeeping columns (`_order`, `parent_id`, `_locale`) are not filterable.

**Lua:**

```lua
-- Find products where any variant has color "red"
crap.collections.products.find({
    where = {
        ["variants.color"] = "red",
    },
})

-- Group-in-array: filter by a group sub-field within array rows
-- (uses json_extract on the JSON column in the join table)
crap.collections.products.find({
    where = {
        ["variants.dimensions.width"] = "10",
    },
})
```

### Block Sub-Fields

Filter by field values inside block rows. Uses `json_extract` on the block `data` column. Returns parent documents that have **at least one** block row matching. A field inside a layout `row`, `collapsible` or `tabs` is named directly (`content.caption`), and a has-many list or relationship inside the block is read element by element. Groups, nested arrays and nested blocks are followed at any depth; `_block_type` names the type of the block row it follows (`content._block_type`, `content.nested._block_type`) and is refused anywhere else. `content.id` filters by the block row's own id, as `variants.id` does for an array row.

Each block row is read with **its own block type's** fields. When several
block types use a name for fields of different kinds — a number in one, text
in another; a has-many list in one, a single value in another; a text field in
one, a group in another — the filter is read per block type: a row matches
when its own type's field satisfies the operator, negative operators included
(a list inside the row is still read element by element). A path valid for at
least one block type is accepted (`content.info.x` where only one type's
`info` is a group) and matches only rows of the types it is valid for. An
operand that does not fit one type's field (`"high"` against a number) matches
none of that type's rows, and is a validation error only when it fits no
block type. A name every block type defines alike reads the same in every row.

A row whose block type declares **no** field of the filtered name reads the
value as absent (NULL) — whether the declaring types define the name alike or
differently, at the top level and in nested block rows. So
`content.score = { not_exists = true }` and `content.score = { not_in = {} }`
match a document holding such a row, while `exists`, `equals`, `not_equals`
and every other comparison never match on it (a NULL compares to nothing).

**Lua:**

```lua
-- Find posts where any content block has body containing "hello"
crap.collections.posts.find({
    where = {
        ["content.body"] = { contains = "hello" },
    },
})

-- Filter by block type
crap.collections.posts.find({
    where = {
        ["content._block_type"] = "image",
    },
})

-- Group-in-block: filter by a group sub-field within block data
crap.collections.posts.find({
    where = {
        ["content.meta.author"] = "Alice",
    },
})
```

### Has-Many Relationships

Filter by related document IDs (`.id`) of a has-many relationship or upload. The
junction rows are the list's elements, read element by element exactly like a
scalar has-many list (see [Has-many fields](#has-many-fields-element-by-element)):
`{ ["tags.id"] = { not_equals = "tag-123" } }` matches posts that do **not** carry
that tag, including posts with no tags at all.

**Lua:**

```lua
-- Find posts that have tag "tag-123"
crap.collections.posts.find({
    where = {
        ["tags.id"] = "tag-123",
    },
})
```

### Combining Nested and Regular Filters

Nested field filters can be freely combined with regular column filters and OR groups:

**Lua:**

```lua
crap.collections.products.find({
    where = {
        status = "published",
        ["variants.color"] = "red",
        ["or"] = {
            { ["content._block_type"] = "image" },
            { ["tags.id"] = "tag-featured" },
        },
    },
})
```

All filter operators (equals, contains, like, in, greater_than, etc.) work with nested field filters — on both backends, a checkbox inside a row's JSON included (its stored `true`/`false` compares as `1`/`0`, like a top-level checkbox). A `join` field inside a row stores no value and is refused as a filter path, as it is at the top level.

## Full-Text Search

Use the `search` parameter for fast full-text search — powered by SQLite FTS5 or Postgres `tsvector`, depending on the backend. This searches across all text-like fields (text, textarea, richtext, email, code) — including text-like fields inside groups, indexed under their `group__field` column name — or the fields specified in `list_searchable_fields` in the collection's admin config (which may also reference group sub-fields by their `group__field` name). Hidden fields and fields with an `access.read` rule are left out of the default set (the index is shared by every reader); a read-gated field can be listed in `list_searchable_fields` explicitly, a hidden one cannot. Both backends index exactly that set, and the per-document index is kept in sync on every write path — including localized collections, undelete, version restore, and import. Soft-deleted documents stay indexed, so `search` works in the trash view too (the normal view never returns them).

**Lua:**

```lua
local result = crap.collections.posts.find({
    search = "hello world",
    limit = 10,
})
```

**gRPC:**

```bash
grpcurl -plaintext -d '{
    "collection": "posts",
    "search": "hello world",
    "limit": "10"
}' localhost:50051 crap.ContentAPI/Find
```

**Behavior:**
- Each whitespace-separated word is treated as a **prefix** search term (implicit AND): `search = "hel wor"` matches "hello world".
- `search` acts as an additional **filter** on the result set (`id IN (FTS matches)`); ordering follows `order_by` / the default sort as usual — results are **not** re-ranked by FTS relevance.
- `search` can be combined with `where` filters, pagination, sorting, and all other query parameters.
- Collections without text fields silently ignore the `search` parameter.
- The `search` parameter also works with `Count` to get the total number of matching documents.

**Indexed fields** are determined by:
1. `admin.list_searchable_fields` if configured on the collection.
2. Otherwise, all parent-level fields with types: text, textarea, richtext, email, code.

The FTS index is automatically created and rebuilt on server startup for every collection with text fields.

## Valid Filter Fields

A filter or sort on a field the caller cannot read is rejected with an
access error rather than answered — `hidden` fields for everyone, fields
with an `access.read` rule for callers the rule denies, at any depth of the
path (a group, array-row, block or nested-row sub-field included). See
[field-level access](../access-control/field-level.md#filtering-sorting-and-search).

A filter value that does not fit the field's type — a non-numeric string
on a Number field, a non-boolean on a Checkbox — is a validation error
naming the field (400 on every surface), never a silent text comparison.
An access rule's row constraint judged in memory (live events, population
gating) matches nothing for such a value — not even `not_equals` or
`not_in` — just as SQL returns nothing for it.
`like` / `contains` on a Number or Checkbox field match the value's text
form on both backends.

You can filter on any column in the collection table:

- User-defined fields (that have parent columns)
- `id`
- `created_at` (if timestamps enabled)
- `updated_at` (if timestamps enabled)

System columns (names starting with `_`, such as `_status`, `_deleted_at`, `_ref_count`, `_locked`, `_password_hash`) are engine-internal and cannot be filtered on directly. Reach their data via typed request flags (`draft = true`, `trash = true`) instead.

Additionally, you can filter on sub-fields using dot notation:

- **Group sub-fields:** `group_name.sub_field` (syntactic sugar for `group_name__sub_field`); an array, blocks or has-many field inside a group continues from `group_name.field` or `group_name__field` (`seo.links.url`, `seo__tags.id`)
- **Array sub-fields:** `array_name.sub_field`, `array_name.id` (the row's id), or a path into a group, nested array or nested blocks sub-field at any depth (`array_name.group.sub_field`, `array_name.nested.sub_field`)
- **Block sub-fields:** `blocks_name.field`, `blocks_name._block_type`, `blocks_name.id` (the row's id), or a path into a group, nested array or nested blocks at any depth (`blocks_name.group.sub_field`)
- **Has-many relationships:** `relationship_name.id`
