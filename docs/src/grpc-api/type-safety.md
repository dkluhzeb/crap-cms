# Type Safety

The gRPC API carries document fields in a `DataMap` — a `map<string, FieldValue>`
keyed by the Lua field name, with no per-collection message at the proto level.
This is a deliberate design choice: Lua files define schemas, the proto stays
stable, and the binary never needs recompiling when you add a field (field
*names* never appear in the proto).

`DataMap`/`FieldValue` replace the older `google.protobuf.Struct`. The values are
typed — a `FieldValue` is a `oneof` over the JSON-shaped value kinds — but the
*map itself* is still schemaless: your gRPC client looks values up by name and
sees a generic `map`. This page explains how to get per-collection type safety
back on top of it.

## FieldValue

Document content is a tree of `FieldValue`s. Each one is a `oneof` over the
JSON-shaped value kinds:

```protobuf
// An object of field values (keys are the Lua field names).
message DataMap {
  map<string, FieldValue> fields = 1;
}

// An ordered list of field values (array / blocks rows, etc.).
message FieldList {
  repeated FieldValue values = 1;
}

message FieldValue {
  oneof kind {
    google.protobuf.NullValue null_value = 1;
    double double_value = 2;
    string string_value = 3;
    bool bool_value = 4;
    DataMap struct_value = 5;   // nested object
    FieldList list_value = 6;   // array
    int64 int_value = 7;
  }
}
```

A producer sets exactly one variant per value. Read a value via the oneof
accessor for its kind: `string_value` for text, `bool_value` for checkboxes,
`struct_value` (a nested `DataMap`) for groups, `list_value` (a `FieldList`) for
arrays/blocks, and `null_value` for null. **Numbers split into two variants** —
whole numbers arrive as `int_value` (an exact `int64`), fractional ones as
`double_value` — so integers keep full precision on the wire (the old `Struct`
path carried every number as a `double`, silently rounding integers above 2^53).

## The Two-Layer Architecture

```
┌──────────────────────────────────────────────────────────┐
│  Lua definitions (source of truth)                       │
│  collections/posts.lua → fields, types, options          │
└──────────┬────────────────┬────────────────┬─────────────┘
           │                │                │
    ┌──────▼──────┐  ┌──────▼──────────┐  ┌──▼─────────────┐
    │ Describe-   │  │ crap-cms typegen│  │ crap-cms typegen│
    │ Collection  │  │ client -l X     │  │ lua            │
    │ (runtime,   │  │ (build-time)    │  │ (build-time)   │
    │  gRPC)      │  │                 │  │                │
    └──────┬──────┘  └──────┬──────────┘  └──┬─────────────┘
           │                │                │
    ┌──────▼──────┐  ┌──────▼──────────┐  ┌──▼─────────────┐
    │ Generic     │  │ types/client.X  │  │ types/crap.lua │
    │ Document    │  │ (TS/Go/Py/Rs    │  │ types/hooks.lua│
    │ message     │  │ typed shapes    │  │ (IDE types for │
    │             │  │ for API clients)│  │ hooks/init.lua)│
    └─────────────┘  └─────────────────┘  └────────────────┘
```

**Layer 1: Runtime schema discovery** — the `DescribeCollection` RPC returns the full field schema. gRPC clients call it at startup or build time to generate typed wrappers.

**Layer 2: Server-side Lua typegen** — `crap-cms typegen lua` writes `types/crap.lua` (the `crap.*` API surface) and `types/hooks.lua` (per-collection hook/data/doc shapes) with LuaLS annotations. This gives you autocompletion and type checking inside hooks and init.lua. Under `admin.dev_mode = true`, `crap-cms serve` regenerates them on every startup.

**Layer 3: Client-side consumer typegen** — `crap-cms typegen client -l <lang>` writes `types/client.<ext>` with typed per-collection shapes for external API consumers (TypeScript, Go, Python, Rust).

## DescribeCollection

The `DescribeCollection` RPC returns the full schema for any collection or global:

```bash
grpcurl -plaintext -d '{"slug": "posts"}' \
    localhost:50051 crap.ContentAPI/DescribeCollection
```

Response:

```json
{
  "slug": "posts",
  "singularLabel": "Post",
  "pluralLabel": "Posts",
  "timestamps": true,
  "fields": [
    {
      "name": "title",
      "type": "text",
      "required": true,
      "unique": true
    },
    {
      "name": "slug",
      "type": "text",
      "required": true,
      "unique": true
    },
    {
      "name": "status",
      "type": "select",
      "required": true,
      "options": [
        { "label": "Draft", "value": "draft" },
        { "label": "Published", "value": "published" },
        { "label": "Archived", "value": "archived" }
      ]
    },
    {
      "name": "content",
      "type": "richtext"
    },
    {
      "name": "author",
      "type": "relationship",
      "relationshipCollection": "users",
      "relationshipMaxDepth": 1
    },
    {
      "name": "tags",
      "type": "relationship",
      "relationshipCollection": "tags",
      "relationshipHasMany": true
    }
  ]
}
```

### FieldInfo Schema

Each field in the response has:

| Field | Type | Description |
|-------|------|-------------|
| `name` | string | Column name |
| `type` | string | Field type: `text`, `number`, `select`, `relationship`, etc. |
| `required` | bool | Whether the field is required |
| `unique` | bool | Whether the field has a uniqueness constraint |
| `options` | SelectOptionInfo[] | Options for `select` fields (label + value) |
| `relationship_collection` | string? | Target collection slug for `relationship` fields |
| `relationship_has_many` | bool? | Whether it's a many-to-many relationship |
| `relationship_max_depth` | int? | Per-field population depth cap |
| `fields` | FieldInfo[] | Sub-fields for `array` and `group` types (recursive) |

## Building Typed Clients

The idea: call `DescribeCollection` once (at build time or app startup), then generate typed wrappers for your language.

> The hand-rolled examples below are the minimal, do-it-yourself mapping —
> useful to understand the shape, and all you need for a depth-0 client. For a
> turnkey generator that already models population depth, narrows selects, and
> types polymorphic relationships, use the built-in
> [`typegen client`](#generated-client-types-typegen-client) instead.

### TypeScript Example

Call `DescribeCollection` for each collection and generate interfaces:

```typescript
// Generated from DescribeCollection("posts")
interface Post {
  id: string;
  title: string;
  slug: string;
  status: "draft" | "published" | "archived";
  content?: string;
  author?: string;        // relationship ID (depth=0)
  tags?: string[];         // has_many relationship IDs
  created_at?: string;
  updated_at?: string;
}

interface CreatePostInput {
  title: string;           // required
  slug: string;            // required
  status: string;          // required
  content?: string;
  author?: string;
  tags?: string[];
}
```

The mapping from `FieldInfo.type` to TypeScript types:

```typescript
function fieldTypeToTS(field: FieldInfo): string {
  switch (field.type) {
    case "text":
    case "textarea":
    case "richtext":
    case "email":
    case "date":
    case "slug":
      return "string";
    case "number":
      return "number";
    case "checkbox":
      return "boolean";
    case "json":
      return "unknown";
    case "select":
      return field.options.map(o => `"${o.value}"`).join(" | ");
    case "relationship":
      return field.relationshipHasMany ? "string[]" : "string";
    case "array":
      // Recurse into sub-fields
      return `Array<{ ${field.fields.map(f =>
        `${f.name}${f.required ? '' : '?'}: ${fieldTypeToTS(f)}`
      ).join('; ')} }>`;
    default:
      return "unknown";
  }
}
```

Because `fields` is a `DataMap` of `FieldValue` oneofs (not a plain object),
decode each value by its set variant before mapping to your typed shape:

```typescript
// Collapse a FieldValue oneof to a plain JS value.
function decodeValue(v: FieldValue): unknown {
  switch (v.kind?.$case) {
    case "nullValue":   return null;
    case "intValue":    return Number(v.kind.intValue);    // int64 -> number (bigint if huge)
    case "doubleValue": return v.kind.doubleValue;
    case "stringValue": return v.kind.stringValue;
    case "boolValue":   return v.kind.boolValue;
    case "structValue": return decodeFields(v.kind.structValue);         // nested DataMap
    case "listValue":   return v.kind.listValue.values.map(decodeValue); // FieldList
    default:            return undefined;
  }
}

// Turn a DataMap into a plain { [name]: value } object.
function decodeFields(m: DataMap): Record<string, unknown> {
  return Object.fromEntries(
    Object.entries(m.fields).map(([k, v]) => [k, decodeValue(v)]),
  );
}
```

> The exact oneof accessor shape (`v.kind?.$case`, `v.getStringValue()`, a
> discriminated union, …) depends on which TS gRPC codegen you use (ts-proto,
> google-protobuf, `@grpc/proto-loader`, …). Adjust the switch to your
> generator; the variant *names* (`int_value`, `string_value`, …) are fixed by
> the proto.

A typed wrapper around the gRPC client:

```typescript
// Wrap the untyped gRPC client with generated types
class PostsClient {
  constructor(private client: ContentAPIClient) {}

  async find(query?: FindQuery): Promise<{ documents: Post[]; total: number }> {
    const resp = await this.client.find({ collection: "posts", ...query });
    return {
      documents: resp.documents.map(d => ({ id: d.id, ...decodeFields(d.fields) } as Post)),
      total: resp.pagination.total_docs,
    };
  }

  async create(data: CreatePostInput): Promise<Post> {
    // The inverse of decodeFields: wrap each input value in a FieldValue
    // (int_value for whole numbers, string_value for text, ...) to build the DataMap.
    const resp = await this.client.create({ collection: "posts", data: encodeFields(data) });
    return { id: resp.document.id, ...decodeFields(resp.document.fields) } as Post;
  }
}
```

### Go Example

Same pattern — `DescribeCollection` at build time, generate structs:

```go
// Generated from DescribeCollection("posts")
type Post struct {
    ID        string  `json:"id"`
    Title     string  `json:"title"`
    Slug      string  `json:"slug"`
    Status    string  `json:"status"`
    Content   *string `json:"content,omitempty"`
    Author    *string `json:"author,omitempty"`
    CreatedAt *string `json:"created_at,omitempty"`
    UpdatedAt *string `json:"updated_at,omitempty"`
}

// Convert a generic Document to a typed Post.
// doc.Fields is a *crap.DataMap; its .Fields is map[string]*crap.FieldValue.
func DocumentToPost(doc *crap.Document) Post {
    p := Post{ID: doc.Id}
    if f := doc.Fields.Fields; f != nil {
        if v, ok := f["title"]; ok {
            p.Title = v.GetStringValue() // oneof accessor for string_value
        }
        // Numbers: use v.GetIntValue() for whole numbers (int64, exact) and
        // v.GetDoubleValue() for fractional ones. Type-switch on v.GetKind()
        // (*crap.FieldValue_IntValue, *crap.FieldValue_StringValue, ...) to tell
        // which variant is set. Nested objects come back as v.GetStructValue()
        // (a *crap.DataMap), lists as v.GetListValue() (a *crap.FieldList).
    }
    return p
}
```

### Python Example

```python
# Generated from DescribeCollection("posts")
from dataclasses import dataclass
from typing import Optional, List

@dataclass
class Post:
    id: str
    title: str
    slug: str
    status: str  # "draft" | "published" | "archived"
    content: Optional[str] = None
    author: Optional[str] = None
    tags: Optional[List[str]] = None
    created_at: Optional[str] = None
    updated_at: Optional[str] = None

def document_to_post(doc) -> Post:
    # doc.fields is a DataMap; the field map lives on doc.fields.fields,
    # and each value is a FieldValue with a `kind` oneof.
    fields = doc.fields.fields
    return Post(
        id=doc.id,
        title=fields["title"].string_value,
        slug=fields["slug"].string_value,
        status=fields["status"].string_value,
        content=fields["content"].string_value or None,
        # Numbers: read fields[name].int_value for whole numbers (exact) and
        # fields[name].double_value for fractional ones. fields[name].WhichOneof("kind")
        # tells you which variant is set ("int_value", "string_value", "null_value", ...).
    )
```

## Generated client types (`typegen client`)

Rather than hand-rolling the wrappers above, `crap-cms typegen client -l <lang>`
emits them for you — `types/client.{ts,go,py,rs}` — walking your Lua schema the
same way the Lua typegen does. Regenerate after any schema change (or a binary
upgrade):

```bash
crap-cms typegen client -l ts,go,py,rs
```

Each **collection** gets a `…Data` type (writable input fields) and a
`…Document` type (adds `id` + timestamps); each **global** gets a single type.
The field mapping is richer than the minimal `fieldTypeToTS` above — it models
population depth, narrows selects, and types polymorphic relationships:

| Schema field | Rust | Go | TypeScript | Python |
|---|---|---|---|---|
| `text` / `richtext` / `date` / … | `String` | `string` | `string` | `str` |
| `number` | `f64` | `float64` | `number` | `float` |
| `checkbox` | `bool` | `bool` | `boolean` | `bool` |
| `select` | `enum { …, Other(String) }` | `type X string` + consts | `"a" \| "b"` | `Literal["a", "b"]` |
| relationship / upload (single) | `Rel<T>` | `Rel[T]` | `string \| TDocument` | `str \| T` |
| relationship / upload (has-many) | `Vec<Rel<T>>` | `[]Rel[T]` | `(string \| TDocument)[]` | `list[str \| T]` |
| polymorphic relationship | untagged `enum` + tagged ref enum | `interface{}` | `string \| ADocument \| BDocument` | `str \| A \| B` |

Key semantics baked into these types:

- **Relationships follow `depth`.** `Rel<T>` (and its per-language equivalents)
  is *either* an id string (`depth = 0`) *or* the populated document
  (`depth >= 1`) — the type never lies about which you get. Rust and Go decode
  both JSON forms automatically (Rust via `#[serde(untagged)]`, Go via a custom
  `UnmarshalJSON`); TS/Python are a union you narrow with a
  `typeof x === "string"` / `isinstance(x, str)` check.
- **A single relationship is optional.** A non-`has_many` relationship/upload is
  optional on read even when `required` on write, because it can be absent after
  the target is soft-deleted or access-denied. Handle the empty case.
- **`select` is lossless in Rust and Go.** A value dropped from the schema after
  you generated still deserializes (`Other(String)` in Rust, a bare `string`
  newtype in Go) instead of erroring; TypeScript and Python narrow to the known
  set.
- **`CollectionSlug`** enumerates every collection slug — a named type with
  constants in Rust/Go, a string-literal union in TS/Python.
- **Name collisions fail generation.** If two constructs would produce the same
  type name (a collection slugged `posts_status` and the `status` select of a
  `posts` collection both map to `PostsStatus`), the command errors instead of
  emitting one wrong type — rename one.

### Rust: decoding the gRPC wire (`typegen proto`)

The other three languages emit *type definitions only* — pair them with your own
gRPC codegen and decode the `DataMap`/`FieldValue` wire yourself (see the
examples above). Rust additionally has `crap-cms typegen proto`, which generates
`From<proto::Document>` (`FromDocument`) impls that decode the typed wire
straight into the `typegen client -l rs` structs — including a **populated**
relationship (`Rel::Doc`) nested inside a group/array/blocks at any depth.
Regenerate the two together; they are designed to compile as one module:

```bash
crap-cms typegen client -l rs           --output src/generated
crap-cms typegen proto  --module crate::proto --output src/generated
```

## Lua Typegen (for Hooks)

The gRPC type safety story above is for **external clients**. For **Lua hooks and init.lua**, the built-in typegen provides IDE-level type safety.

### Generate Types

Under `admin.dev_mode = true`, server-side Lua types are auto-regenerated on every `crap-cms serve` startup. In production (or to refresh after a binary upgrade), regenerate explicitly:

```bash
crap-cms typegen lua
```

This writes `<config_dir>/types/crap.lua` (the `crap.*` API surface, copied from the binary) and `<config_dir>/types/hooks.lua` (per-collection hook/data/doc shapes derived from your collection definitions) with LuaLS annotations.

For external API consumers (TypeScript, Go, Python, Rust), use the `client` subcommand:

```bash
crap-cms typegen client -l ts,go,py,rs
```

### What Gets Generated

For each collection, typegen emits:

| Type | Purpose |
|------|---------|
| `crap.data.Posts` | Input fields (for Create/Update data) |
| `crap.doc.Posts` | Full document (fields + id + timestamps) |
| `crap.hook.Posts` | Typed hook context (`collection`, `operation`, `data`) |
| `crap.find_result.Posts` | Find result (`documents[]` + `total`) |
| `crap.filters.Posts` | Filter keys for queries |
| `crap.query.Posts` | Query options (filters, order_by, limit, offset) |
| `crap.hook_fn.Posts` | Hook function signature |

For globals: `crap.global_data.*`, `crap.global_doc.*`, `crap.hook.global_*`.

For array fields: `crap.array_row.*` with the sub-field types.

Select fields become union types: `"draft" | "published" | "archived"`.

Function overloads are generated so `crap.collections.posts.find(...)` returns `crap.find_result.Posts` instead of the generic `crap.FindResult`.

### IDE Setup

Add a `.luarc.json` in your config directory:

```json
{
  "runtime": { "version": "Lua 5.4" },
  "workspace": { "library": ["./types"] }
}
```

LuaLS (used by VS Code, Neovim, etc.) will then provide:

- Autocompletion on all document fields
- Type checking for field values
- Inline errors for typos and type mismatches
- Hover documentation showing field types
- Smart overloads on `crap.collections.find()` per collection

### Example Generated Output

For a `posts` collection with `title`, `slug`, `status` (select), `content` (richtext):

```lua
---@class crap.data.Posts
---@field title string
---@field slug string
---@field status "draft" | "published" | "archived"
---@field content? string

---@class crap.doc.Posts
---@field id string
---@field title string
---@field slug string
---@field status "draft" | "published" | "archived"
---@field content? string
---@field created_at? string
---@field updated_at? string

---@class crap.hook.Posts
---@field collection "posts"
---@field operation "create" | "update"
---@field data crap.data.Posts
```

## Why a schemaless DataMap?

The `Document.fields` is a `DataMap` (`map<string, FieldValue>`, not
per-collection messages) because:

1. **Single binary** — the proto file is compiled into the binary. Per-collection proto messages would require recompilation when schemas change.
2. **Lua is the schema source** — schemas live in Lua files, not proto definitions. The proto layer is a transport, not a schema system.
3. **Dynamic schemas** — collections can be added, removed, or modified by editing Lua files without touching the binary or proto. Field *names* never appear in the proto, so adding a field is not a wire change.
4. **DescribeCollection fills the gap** — runtime schema discovery gives clients everything they need to build typed wrappers, without coupling the proto to specific schemas.

`DataMap`/`FieldValue` keep all four properties — the map is still keyed by
name and schemaless at the proto level — while making the *values* typed and
precision-safe. Unlike the older `google.protobuf.Struct`, whose only numeric
kind is a `double` (silently rounding integers above 2^53 ~ 9.0e15),
`FieldValue` carries an explicit `int64` (`int_value`) alongside
`double_value`, so integers survive the round trip exactly.
