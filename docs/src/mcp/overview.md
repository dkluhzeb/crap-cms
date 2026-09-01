# MCP (Model Context Protocol)

Crap CMS includes a built-in MCP server that lets AI assistants (Claude Desktop,
Cursor, VS Code extensions, custom agents) interact with your CMS content and schema.

The MCP server **auto-generates** tool definitions from your Lua-defined collections
and globals. Any CMS instance automatically gets a full MCP API matching its schema.

## Configuration

Add an `[mcp]` section to `crap.toml`:

```toml
[mcp]
enabled = true              # Enable MCP server (default: false)
http = false                # Enable HTTP transport on /mcp (default: false)
config_tools = false        # Enable config generation tools (default: false)
api_key = ""                # API key for HTTP auth (required, min 32 chars, when http = true)
http_max_body_bytes = "1MB" # Max /mcp request-body size (int bytes or "16MB"-style string)
include_collections = []    # Whitelist (empty = all)
exclude_collections = []    # Blacklist (takes precedence over include)
```

## Transports

### stdio (default)

Run the MCP server as a subprocess that reads JSON-RPC from stdin and writes to stdout:

```bash
crap-cms mcp
```

Or from outside the config directory:

```bash
crap-cms mcp -C /path/to/config
```

For **Claude Code** (CLI / IDE extensions), add via the CLI:

```bash
claude mcp add-json my-cms '{"type":"stdio","command":"crap-cms","args":["-C","/path/to/config","mcp"]}' --scope local
```

Or create a `.mcp.json` in your project root (shared with team via version control):

```json
{
  "mcpServers": {
    "my-cms": {
      "type": "stdio",
      "command": "crap-cms",
      "args": ["-C", "/path/to/config", "mcp"]
    }
  }
}
```

The `-C` path must point to a directory containing `crap.toml`. Use an absolute path or a path relative to where Claude Code is launched from.

`crap-cms init` automatically generates a `.mcp.json` in the config directory, so new projects work with Claude Code out of the box.

Verify with `claude mcp list`.

For **Claude Desktop**, add to your `claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "my-cms": {
      "command": "crap-cms",
      "args": ["mcp", "-C", "/path/to/config"]
    }
  }
}
```

### HTTP

When `mcp.http = true`, the admin server exposes a `POST /mcp` endpoint.
Send JSON-RPC 2.0 requests as the request body.

An `api_key` is **required** when HTTP transport is enabled, and must be at least
32 characters. If `mcp.http = true` and `api_key` is empty or too short, the server
**fails to start** with a config-validation error (generate a key with
`openssl rand -hex 32`). Requests must include an `Authorization: Bearer <key>`
header; a missing or wrong key is answered with a JSON-RPC error (code `-32600`,
"Invalid or missing API key") over HTTP `200`, not an HTTP `401`.

Request bodies are capped at `[mcp] http_max_body_bytes` (default **1 MiB**;
larger bodies get a JSON-RPC parse error). Raise it when clients push large
payloads — bulk creates, or `write_config_file` with big assets. A JSON-RPC
**notification** (a request without an `id`) is executed and answered with
HTTP `204 No Content`, per the JSON-RPC convention of not responding to
notifications.

## Auto-Generated Tools

### Content CRUD (per collection)

For each collection (e.g., `posts`), a set of CRUD tools is generated:

| Tool | Description |
|------|-------------|
| `find_posts` | Query documents with filters, ordering, pagination |
| `find_by_id_posts` | Get a single document by ID |
| `count_posts` | Count documents matching filters |
| `create_posts` | Create a new document |
| `create_many_posts` | Bulk create documents in batched transactions |
| `update_posts` | Update an existing document |
| `update_many_posts` | Bulk update documents matching a filter |
| `validate_posts` | Validate document data without persisting — returns per-field errors |
| `delete_posts` | Delete a document |
| `delete_many_posts` | Bulk delete documents matching a filter |

Collections with `soft_delete` also get `undelete_posts`; versioned collections
add `unpublish_posts`, `list_versions_posts` (args: `id`, optional `limit` /
`offset`), and `restore_version_posts` (args: `id`, `version_id`).

> **Reserved slug prefixes.** Because tool names are built as `{op}_{slug}` and
> `{op}` includes the compound forms `create_many_` / `update_many_` /
> `delete_many_` / `find_by_id_`, a collection or global slug **may not begin with
> `many_` or `by_id_`** — those would collide with a bulk/by-id tool name. A slug
> that does is rejected at load time, the same way an invalid slug is.

`validate_*` runs the full before-write pipeline (field coercion, validators,
unique checks, `before_validate` hooks) and reports per-field errors without
writing a row — the dry-run runs inside a transaction that is always rolled
back, and with the same trusted override as MCP's real writes, so its outcome
predicts exactly what the actual `create_*`/`update_*` call would do. Pass an
`id` to validate in update mode (the row is excluded from unique checks); omit
it to validate in create mode.

Input schemas are generated from your field definitions. Required fields, select
options, and relationship types are all reflected in the JSON Schema.

Write tools are **strict about data keys**: an argument that is neither a declared
top-level field (layout wrappers like Row/Collapsible/Tabs are transparent — their
sub-fields count as top-level) nor a reserved argument (see below) is **rejected**
with an error, rather than silently ignored. A misspelled field name fails loudly
instead of quietly writing nothing. `update_many` additionally **rejects a
`password` key** (it applies one value to many rows); `create_many` **accepts** a
per-item `password` on auth collections, validated against the password policy and
hashed per document — parity with the single `create_*` tool.

Alongside field values, the write tools accept a few **reserved top-level
arguments** (excluded from the document's field data, like `id` and
`password`):

| Argument | Tools | Description |
|----------|-------|-------------|
| `locale` | `create_*`, `create_many_*`, `update_*`, `update_many_*`, `validate_*`, `global_read_*`, `global_update_*`, `global_validate_*` | Locale code for localized fields. |
| `draft` | `create_*`, `create_many_*`, `update_*`, `update_many_*`, `validate_*`, `global_update_*`, `global_validate_*` | Write as a draft version. |
| `events` | all write tools | Publish live events for this write. Defaults to `true` on single-document tools and `false` on the bulk (`*_many_*`) tools. |
| `hooks` | `create_many_*`, `update_many_*`, `delete_many_*` | Run lifecycle hooks per item (default `true`). Bulk-only; single-document tools always run hooks. |
| `force_hard_delete` | `delete_*`, `delete_many_*` | Skip `soft_delete` and remove the row permanently. |

> A collection with a field literally named `locale`, `draft`, `events`, or
> `force_hard_delete` would have it shadowed by the reserved argument — the
> same caveat that already applies to `id` and `password`.

### Global CRUD (per global)

For each global (e.g., `settings`):

| Tool | Description |
|------|-------------|
| `global_read_settings` | Read the global document |
| `global_update_settings` | Update the global document |
| `global_validate_settings` | Validate global data without persisting — returns per-field errors |

### Schema Introspection

Always available:

| Tool | Description |
|------|-------------|
| `list_collections` | List all collections with their labels and capabilities |
| `describe_collection` | Get full field schema for a collection or global |
| `list_field_types` | List all field types with descriptions and capabilities |
| `cli_reference` | Get CLI command reference (all or specific command) |

### Config Generation Tools (opt-in)

When `config_tools = true`:

| Tool | Description |
|------|-------------|
| `read_config_file` | Read a file from the config directory (secrets in `crap.toml` are redacted) |
| `write_config_file` | Write **any file** (Lua, templates, static assets, `crap.toml`, …) inside the config directory. Path-traversal-safe (confined to the config dir) but not restricted by file type |
| `list_config_files` | List files in the config directory |

These are opt-in because they allow arbitrary file writes inside the config directory — which includes executable Lua (hooks, jobs, routes) that the server runs. Enable them only for trusted MCP clients.

`read_config_file` **redacts secrets when it reads `crap.toml`** — `auth.secret`,
`email.smtp_pass`, `mcp.api_key`, and the S3 `secret_key` come back masked, the
same values sanitized from the `crap://config` resource. Secrets never leave the
server through the MCP surface.

## MCP Descriptions

Add optional `mcp` tables to your Lua definitions to provide context for AI assistants:

### Collection level

```lua
crap.collections.define("posts", {
  mcp = {
    description = "Blog posts with title, content, and author relationship",
  },
  fields = { ... }
})
```

The collection `description` is appended to **every** generated tool for
that collection (`find`, `create`, `delete`, …), not just one — so the
AI sees the collection's context whichever operation it is calling.

### Field level

```lua
crap.fields.select({
  name = "status",
  mcp = {
    description = "Publication status - controls visibility on the frontend",
  },
  options = { ... },
})
```

If no `mcp.description` is set, the tool falls back to `admin.description`
(for fields) or a generated description based on the collection label.

### Per-operation descriptions

Each generated tool gets an auto-written description from the collection
label, enriched with the collection's own semantics so the AI knows the
non-obvious behavior without configuration:

- **drafts enabled** → `create` / `create_many` / `update` note that
  `draft=true` saves an unpublished draft.
- **soft-delete enabled** → `delete` / `delete_many` note that the
  default is a soft delete and `force_hard_delete=true` removes
  permanently.

To override the description for a specific operation, set
`mcp.operations` keyed by operation name. The override replaces the
auto-generated body; the collection-level `description` is still
appended after it.

```lua
crap.collections.define("posts", {
  mcp = {
    description = "Blog posts.",
    operations = {
      delete = "Archive a post. Soft-deletes by default; force_hard_delete purges it.",
      create = "Draft a new post — pass draft=true to keep it unpublished.",
    },
  },
  fields = { ... }
})
```

Valid collection operation keys: `find`, `find_by_id`, `create`,
`create_many`, `update`, `update_many`, `validate`, `delete`,
`delete_many`, `count`, `undelete`, `unpublish`, `list_versions`,
`restore_version`. For **globals** the keys are `read`, `update`, and
`validate`. An unknown key is **rejected at load time** (the definition
fails to load), matching the strict validation used elsewhere in the schema.

## Collection Filtering

Use `include_collections` and `exclude_collections` to control which collections
are exposed via MCP:

```toml
[mcp]
enabled = true
exclude_collections = ["users"]  # Hide sensitive collections
```

`exclude_collections` takes precedence when a collection appears in both lists. Both lists are matched by **slug** and apply to globals as well as collections: a non-empty `include_collections` exposes *only* the listed slugs (list your globals too), and an excluded global disappears from tool listing, execution and the schema resources alike.

## Security & Access Model

MCP operates with **full access** — collection-level and field-level access control
functions are not applied. This is by design: MCP is a machine-to-machine API surface
(equivalent to Lua's `override_access = true`), gated by transport-level authentication:

- **stdio:** Access is controlled by who can run the process.
- **HTTP:** Access is controlled by the `api_key` setting (minimum 32 characters).
  An API key is **required** when `http = true`: if it is empty or too short,
  `crap-cms` refuses to start with a config-validation error, so a misconfigured
  HTTP endpoint cannot come up unprotected. At runtime a request with a missing or
  wrong key receives a JSON-RPC error response ("Invalid or missing API key"), and
  the key is compared in constant time.

To restrict which collections are accessible, use `include_collections` /
`exclude_collections`. These filters are enforced both in tool listing (`tools/list`)
and at execution time, so knowing a collection slug is not enough to bypass the filter.

A second, per-collection control is the **`access.mcp`** rule (set on the
collection or global definition). Unlike per-user access (which MCP bypasses),
`access.mcp` is a user-independent boolean gate evaluated at startup: it decides
whether the collection is exposed to the MCP surface *at all* — its tools,
resources, and schema introspection. The default is permissive (a collection is
exposed when MCP is enabled globally), so `access.mcp` only ever *removes* a
collection — e.g. keep an internal collection out of the LLM surface while still
serving it over the admin/gRPC APIs. It must return `true`/`false`; a filter
table is a configuration error (the shared-key surface has no per-user identity
to scope against), and a deny — or any evaluation error — fails closed (the
collection is hidden, and execution re-checks the gate independently of listing).
`access.mcp` and the `include_collections` / `exclude_collections` filters
compose: a collection is reachable only if it passes both.

All MCP write operations (create, update, delete) are logged at `info` level for
audit purposes. Hooks still fire on all MCP writes (same lifecycle as admin/gRPC).

## Resources

The MCP server also exposes read-only resources:

| URI | Description |
|-----|-------------|
| `crap://schema/collections` | Full schema of all collections as JSON |
| `crap://schema/globals` | Full schema of all globals as JSON |
| `crap://config` | Current configuration (secrets sanitized: `auth.secret`, `email.smtp_pass`, `mcp.api_key`, `upload.s3.secret_key`) |

## Query Parameters

The `find_*` tools accept these parameters:

| Parameter | Type | Description |
|-----------|------|-------------|
| `where` | object | Filter conditions (same syntax as gRPC/Lua API) |
| `order_by` | string | Sort field (prefix with `-` for descending, e.g., `"-created_at"`) |
| `limit` | integer | Max results per page |
| `page` | integer | Page number, 1-indexed (page mode only) |
| `after_cursor` | string | Forward cursor (cursor mode only, mutually exclusive with `page` and `before_cursor`) |
| `before_cursor` | string | Backward cursor (cursor mode only, mutually exclusive with `page` and `after_cursor`) |
| `depth` | integer | Relationship population depth |
| `search` | string | Full-text search query |
| `select` | string[] | Field names to return (projection); omit for all fields |

`count_*` accepts `where`, `search`, `locale`, `draft`, and `trash` — the
same query a `find_*` call matches. `unpublish_*` and `undelete_*` accept an
`events` boolean (default `true`) for quiet writes.

### Response Format

`find_*` tools return a JSON object with `docs` and `pagination`:

```json
{
  "docs": [
    { "id": "abc123", "title": "Hello World", "created_at": "2026-01-15T09:00:00Z" }
  ],
  "pagination": {
    "total_docs": 25,
    "limit": 10,
    "has_next_page": true,
    "has_prev_page": false,
    "total_pages": 3,
    "page": 1,
    "page_start": 1,
    "next_page": 2
  }
}
```

In cursor mode, `page`/`total_pages`/`page_start`/`next_page`/`prev_page` are replaced by `start_cursor`/`end_cursor`.

### Where clause example

```json
{
  "name": "find_posts",
  "arguments": {
    "where": {
      "status": { "equals": "published" },
      "created_at": { "greater_than": "2024-01-01" }
    },
    "order_by": "-created_at",
    "limit": 10
  }
}
```

Supported operators: `equals`, `not_equals`, `greater_than`, `greater_than_or_equal`,
`less_than`, `less_than_or_equal`, `like`, `contains`, `in` (array), `not_in` (array),
`exists`, `not_exists`.

A malformed clause is **rejected loudly**, never silently dropped: an unknown
operator, an `in` / `not_in` whose value is not an array, or a bare array as the
whole condition all return an error. This matters most for `delete_many` /
`update_many` — a filter that fails to parse must never fall through to "match
everything".

> **Note:** Operator names are the **one grammar** shared by every surface (the
> gRPC/JSON `where` API, the admin list URL, MCP, and the Lua filter
> representation) — see [`FilterOp::op_name`]. (Earlier alphas spelled the ordered
> operators differently per surface, e.g. the admin URL's `gte` and MCP's
> `greater_than_equal`; those short forms are gone.)
