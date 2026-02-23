# Type Safety

The gRPC API uses `google.protobuf.Struct` for document fields — a generic JSON object with no schema at the proto level. This is a deliberate design choice: Lua files define schemas, the proto stays stable, and the binary never needs recompiling when you add a field.

But `Struct` means your gRPC client sees `fields` as an untyped map. This page explains how to get type safety back.

## The Two-Layer Architecture

```
┌──────────────────────────────────────────────────┐
│  Lua definitions (source of truth)               │
│  collections/posts.lua → fields, types, options  │
└────────────┬─────────────────────┬───────────────┘
             │                     │
    ┌────────▼────────┐   ┌───────▼────────────┐
    │  DescribeCollection │   │  --generate-types    │
    │  (runtime, gRPC)    │   │  (build-time, Lua)   │
    └────────┬────────┘   └───────┬────────────┘
             │                     │
    ┌────────▼────────┐   ┌───────▼────────────┐
    │  Client codegen │   │  types/generated.lua│
    │  TS/Go/Python   │   │  (IDE types for     │
    │  typed wrappers │   │   hooks & init.lua) │
    └─────────────────┘   └────────────────────┘
```

**Layer 1: Runtime schema discovery** — the `DescribeCollection` RPC returns the full field schema. gRPC clients call it at startup or build time to generate typed wrappers.

**Layer 2: Lua typegen** — the `--generate-types` flag writes `types/generated.lua` with LuaLS annotations. This gives you autocompletion and type checking inside hooks and init.lua.

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

A typed wrapper around the gRPC client:

```typescript
// Wrap the untyped gRPC client with generated types
class PostsClient {
  constructor(private client: ContentAPIClient) {}

  async find(query?: FindQuery): Promise<{ documents: Post[]; total: number }> {
    const resp = await this.client.find({ collection: "posts", ...query });
    return {
      documents: resp.documents.map(d => ({ id: d.id, ...d.fields } as Post)),
      total: resp.total,
    };
  }

  async create(data: CreatePostInput): Promise<Post> {
    const resp = await this.client.create({ collection: "posts", data });
    return { id: resp.document.id, ...resp.document.fields } as Post;
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

// Convert a generic Document to a typed Post
func DocumentToPost(doc *crap.Document) Post {
    p := Post{ID: doc.Id}
    if f := doc.Fields.Fields; f != nil {
        if v, ok := f["title"]; ok {
            p.Title = v.GetStringValue()
        }
        // ...
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
    fields = dict(doc.fields)
    return Post(
        id=doc.id,
        title=fields.get("title", {}).string_value,
        slug=fields.get("slug", {}).string_value,
        status=fields.get("status", {}).string_value,
        content=fields.get("content", {}).string_value or None,
        # ...
    )
```

## Lua Typegen (for Hooks)

The gRPC type safety story above is for **external clients**. For **Lua hooks and init.lua**, the built-in typegen provides IDE-level type safety.

### Generate Types

Types are auto-generated on every server startup. You can also generate them explicitly:

```bash
crap-cms --config ./my-project --generate-types
```

This writes `<config_dir>/types/generated.lua` with LuaLS annotations derived from your Lua collection definitions.

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

Function overloads are generated so `crap.collections.find("posts", ...)` returns `crap.find_result.Posts` instead of the generic `crap.FindResult`.

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

## Why Generic Struct?

The `Document.fields` is `google.protobuf.Struct` (not per-collection messages) because:

1. **Single binary** — the proto file is compiled into the binary. Per-collection proto messages would require recompilation when schemas change.
2. **Lua is the schema source** — schemas live in Lua files, not proto definitions. The proto layer is a transport, not a schema system.
3. **Dynamic schemas** — collections can be added, removed, or modified by editing Lua files without touching the binary or proto.
4. **DescribeCollection fills the gap** — runtime schema discovery gives clients everything they need to build typed wrappers, without coupling the proto to specific schemas.
