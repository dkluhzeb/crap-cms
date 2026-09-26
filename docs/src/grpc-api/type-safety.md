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
`struct_value` / `list_value` for `json` fields (their parsed value),
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
| `relationship_collections` | string[] | Target collections of a polymorphic relationship; its values are `collection/id` |
| `has_many` | bool | Whether a `text`, `number` or `select` field holds a list |
| `timezone` | bool | Whether a `date` field carries its IANA timezone in `<name>_tz` (equivalent to `companions` containing `_tz`; kept for clients that read it) |
| `companions` | string[] | Suffixes of the companion keys the field carries beside its value: `_tz` (timezone date), `_lang` (code field with `admin.languages`). Each is an optional string key `<name><suffix>` |
| `localized` | bool | Whether the field stores a value per locale |
| `fields` | FieldInfo[] | Sub-fields for `array` and `group` types (recursive) |
| `blocks` | BlockInfo[] | Block types of a `blocks` field |

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

In TypeScript each **collection** and **global** gets a `…Data` input type and a
`…Document` read type, and each group or array row type gets a `…Data` input
variant beside its read type; Rust, Go and Python emit read types only. The two
are generated from the two different wire shapes — neither is derived from the
other:

- **The input (`…Data`) is what a create or update accepts.** A required field
  stays required — a required relationship included. A relationship or upload
  is its **id** (`string`, `string[]` for has-many; a polymorphic one is the
  `"collection/id"` string), never a document: every write surface rejects a
  populated document. A virtual `join` field is left out (a write can never
  store it), and so are an upload collection's server-derived columns
  (`filename`, `mime_type`, `filesize`, `width`, `height`, `url` and the
  per-size `{size}_url` / `{size}_width` / `{size}_height` / `{size}_{format}_url`
  columns) — the server strips them from every write that is not a file
  upload; `focal_x` / `focal_y` stay writable. An auth collection's input
  carries an optional `password` (hashed on write, never read back; an empty
  one is rejected on create and keeps the stored hash on update — bulk
  updates reject it). A `hidden` field stays in the input: a write may set it.
  An update accepts any subset — `Update<PostsData>`, declared at the top of
  the file — and so does a draft save (`draft: true`) on a collection with
  drafts, which does not enforce required fields. `Update<T>` makes each
  group's sub-fields optional too (a sub-field the update does not send keeps
  its stored value), while an array or blocks row stays whole: send each row
  with its `id` and its required fields. (`Partial<PostsData>` is shallow and
  would demand a group's required sub-fields.) Every optional key also takes `null` (`?: T | null`): an
  absent key keeps the stored value, `null` clears it; a required key and the
  `password` do not, and neither does a group key — a group has no value of
  its own, so clear its sub-fields one by one (`seo: { title: null }`).
- **The read type is what a read returns.** The row type of an array stored in
  its own table (not nested inside another row) has an optional `id` — send it
  back on update to keep the stored row. A read type has `id`, every field
  **optional** and nullable (a draft may lack required values, field read
  access and `select` leave keys out, and an empty value reads as `null` —
  `?: T | null` in TypeScript), the timestamps, and the stored keys the
  collection has: `_revision` (every document — an integer; Go names the
  member `DocumentRevision`), `_status` (drafts; `"draft" | "published"` in
  TypeScript and Python, a plain string in Rust and Go; Go `DraftStatus`),
  `_deleted_at` (soft delete) and
  `<name>_tz` (timezone dates). A `hidden` field is never declared — every read
  strips it, nested ones included.

A collection or global with localized fields also
gets a `locale = "all"` read type — `…LocalizedDocument` in TypeScript,
`…Localized` elsewhere — whose localized fields are per-locale maps. A locale
without a value holds `null`: `Localized<T>` is `{ [locale: string]: T | null }`,
Rust `HashMap<String, Option<T>>`, Go `map[string]*T` (a nil-able value
unchanged) and Python `dict[str, Optional[T]]`.
The field mapping is richer than the minimal `fieldTypeToTS` above — it models
population depth, narrows selects, and types polymorphic relationships:

| Schema field | Rust | Go | TypeScript | Python |
|---|---|---|---|---|
| `text` / `richtext` (HTML) / `date` / … | `String` | `string` | `string` | `str` |
| `richtext` with `admin.format = "json"` | `serde_json::Value` | `interface{}` | `unknown` | `Any` |
| `number` | `f64` | `float64` | `number` | `float` |
| `checkbox` | `bool` | `*bool` | `boolean` | `bool` |
| `select` | `enum { …, Other(String) }` | `type X string` + consts | `"a" \| "b"` | `Literal["a", "b"]` |
| relationship / upload (single), read | `Rel<T>` | `Rel[T]` | `string \| TDocument` | `str \| T` |
| relationship / upload (has-many), read | `Vec<Rel<T>>` | `[]Rel[T]` | `(string \| TDocument)[]` | `list[str \| T]` |
| polymorphic relationship, read | untagged `enum` + tagged ref enum | `interface{}` | `string \| ADocument \| BDocument` | `str \| A \| B` |
| any relationship / upload, input (`…Data`) | — | — | `string` / `string[]` | — |

Key semantics baked into these types:

- **An upload collection's read type carries `sizes`.** The generated document describes the nested `sizes` object a read returns, not the per-size columns it is assembled from — there is no `thumbnail_url` or `thumbnail_width` on the wire. See [Uploads](../uploads/overview.md#api-response).

- **Relationships follow `depth`.** `Rel<T>` (and its per-language equivalents)
  is *either* an id string (`depth = 0`) *or* the populated document
  (`depth >= 1`) — the type never lies about which you get. Rust and Go decode
  both JSON forms automatically (Rust via `#[serde(untagged)]`, Go via a custom
  `UnmarshalJSON`); TS/Python are a union you narrow with a
  `typeof x === "string"` / `isinstance(x, str)` check.
- **A populated document carries its `collection`.** A document embedded as a
  populated relationship has a `collection` key (its slug), declared on every
  collection read type — `collection?: "posts"` in TypeScript,
  `Optional[Literal["posts"]]` in Python, `*string` in Go — so a polymorphic
  union narrows on it (`if (doc.collection === "posts")`). Rust's polymorphic
  enums consume it as their serde tag instead. A collection whose own field is
  named `collection` keeps that field and gets no tag.
- **Every field is optional on read.** Even a `required` field can be absent:
  a draft read returns it empty, field read access and `select` drop it, and a
  single relationship is empty once its target is soft-deleted or
  access-denied. Handle the empty case. Rust wraps each field in `Option`,
  Python defaults it to `None`, and Go reads it through a pointer or a nil-able
  type, so an absent boolean or group is distinguishable from `false` or an
  empty group.
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
Regenerate the two together; they are designed to compile as one module (a
test pins that every decoder builds exactly the fields of the struct it
targets):

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
| `crap.data.Posts` | Hook `ctx.data` — the stored fields (upload metadata included) |
| `crap.input.Posts` | The `create` / `create_many` / `validate` payload — the write shape, required fields required |
| `crap.partial.Posts` | The `update` payload — the write shape, every field optional, a group's sub-fields too (plus `password` on an auth collection) |
| `crap.partial_many.Posts` | The `update_many` payload — as `crap.partial.Posts`, never a `password` (`update_many` refuses one) |
| `crap.doc.Posts` | A returned document — the read shape (id + timestamps) |
| `crap.hook.Posts` | Typed hook context (`collection`, `operation`, `data` as `crap.data.Posts`) |
| `crap.read_hook.Posts` | Typed `after_read` hook context — `data` is the read document, `crap.doc.Posts` |
| `crap.find_result.Posts` | Find result (`documents[]` + `pagination`) |
| `crap.doc_localized.Posts` | A document read with `locale = "all"` — each localized field a `{ [locale] = value }` table (only for a collection with localized fields) |
| `crap.where_group.Posts` | One AND-group of filter conditions: every column except a `hidden` field's, a group's value in both spellings (`seo__title`, `["seo.title"]`), and every path into array/blocks/has-many rows (`["items.label"]`, `["content._block_type"]`, `["tags.id"]`) |
| `crap.where.Posts` | A `where` table: a `crap.where_group.Posts` plus its `["or"]` alternatives |
| `crap.query.Posts` | Query options (`where`, `order_by` — a group's value sortable in both spellings, `limit`, `offset`) |
| `crap.hook_fn.Posts` / `crap.read_hook_fn.Posts` | Hook function signatures |

The write shape (`crap.input`, `crap.partial`, `crap.partial_many`) leaves out
virtual `join` fields (nested ones included) and an upload collection's
server-derived columns, carries each relationship as its id, and — on an auth
collection — `crap.input` and `crap.partial` add an optional `password`. The read shape (`crap.doc`) leaves out `hidden` fields, folds an
upload's per-size columns into `sizes`, types a relationship as its id **or**
the populated `crap.doc.<Target>` (reads populate at the default depth), a JSON
rich text field as the parsed `table`, and declares the `collection` tag a
populated copy carries.

Every optional key of a write class (`crap.input`, `crap.partial`,
`crap.partial_many`, `crap.data`, and their row/group classes) also accepts
`crap.null` — an absent key keeps the stored value, `crap.null` clears it. A
required key of `crap.input` does not: it cannot be cleared. Nor does a group
key: a group has no value of its own, so clear its sub-fields one by one
(`seo = { title = crap.null }`). (The TypeScript `…Data` interfaces do the
same with `| null`.)

On a collection with drafts, `create`, `create_many` and `validate` have an
overload taking the all-optional `crap.partial.<Slug>` with
`{ draft = true }` (`crap.DraftCreateOptions` / `crap.DraftValidateOptions`),
since a draft save does not enforce required fields.

A collection or global with localized fields also gets its `locale = "all"`
read shape: `crap.doc_localized.<Slug>` / `crap.global_doc_localized.<Slug>`
(each localized field a per-locale table, each group holding one its
`crap.doc_group_localized.*`), with `find` / `find_by_id` / `get` overloads
for a `locale = "all"` query (`crap.query_all_locales.<Slug>`,
`crap.AllLocalesFindByIdOptions`, `crap.AllLocalesGlobalGetOptions`). Where
your editor does not pick the overload, cast the result:
`local d = crap.collections.posts.find_by_id(id, { locale = "all" }) --[[@as crap.doc_localized.Posts?]]`.

For globals: `crap.global_data.*`, `crap.global_partial.*`, `crap.global_doc.*`,
`crap.hook.global_*`, `crap.read_hook.global_*`.

For array rows and groups: `crap.array_row.*` / `crap.group.*` in the written
shape, `crap.doc_row.*` / `crap.doc_group.*` in the read shape. A partial write
(`crap.partial`, `crap.partial_many`, `crap.data`, `crap.global_partial`,
`crap.global_data`) types a group as `crap.group_partial.*`, every sub-field
optional — an update keeps each sub-field it does not send. A row is written
whole, so its class (and a group inside it) keeps its required fields.

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
---@class crap.input.Posts
---@field title string
---@field slug string
---@field status "draft" | "published" | "archived"
---@field content? string

---@class crap.doc.Posts : crap.Document
---@field id string
---@field title? string
---@field slug? string
---@field status? "draft" | "published" | "archived"
---@field content? string
---@field collection? "posts" Set when embedded as a populated relationship
---@field created_at? string
---@field updated_at? string

---@class crap.hook.Posts
---@field collection "posts"
---@field operation "create" | "update" | "undelete" | "delete" | "find" | "find_by_id" | "get"
---@field data crap.data.Posts
---@field id? string
---@field context table<string, any>
---@field hook_depth integer
---@field locale? string
---@field draft? boolean
---@field user? table
---@field ui_locale? string
---@field options? table
---@field edited_by? { id: string, email: string }

---@class crap.read_hook.Posts
---@field collection "posts"
---@field operation "find" | "find_by_id" | "get" | "create" | "update" | "delete" | "undelete" | "unpublish" | "restore"
---@field data crap.doc.Posts
---@field id? string
---@field context table<string, any>
---@field hook_depth integer
---@field locale? string
---@field draft? boolean
---@field user? table
---@field ui_locale? string
---@field options? table
---@field edited_by? { id: string, email: string }
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
