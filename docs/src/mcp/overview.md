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
api_key = ""                # API key for HTTP auth (strongly recommended when http = true)
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

For Claude Desktop, add to your `claude_desktop_config.json`:

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

If `mcp.api_key` is set, requests must include an `Authorization: Bearer <key>` header.

## Auto-Generated Tools

### Content CRUD (per collection)

For each collection (e.g., `posts`), five tools are generated:

| Tool | Description |
|------|-------------|
| `find_posts` | Query documents with filters, ordering, pagination |
| `find_by_id_posts` | Get a single document by ID |
| `create_posts` | Create a new document |
| `update_posts` | Update an existing document |
| `delete_posts` | Delete a document |

Input schemas are generated from your field definitions. Required fields, select
options, and relationship types are all reflected in the JSON Schema.

### Global CRUD (per global)

For each global (e.g., `settings`):

| Tool | Description |
|------|-------------|
| `global_read_settings` | Read the global document |
| `global_update_settings` | Update the global document |

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
| `read_config_file` | Read a file from the config directory |
| `write_config_file` | Write a Lua file to the config directory |
| `list_config_files` | List files in the config directory |

These are opt-in because they allow writing to the filesystem.

## MCP Descriptions

Add optional `mcp` tables to your Lua definitions to provide context for AI assistants:

### Collection level

```lua
return {
  slug = "posts",
  mcp = {
    description = "Blog posts with title, content, and author relationship",
  },
  fields = { ... }
}
```

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

## Collection Filtering

Use `include_collections` and `exclude_collections` to control which collections
are exposed via MCP:

```toml
[mcp]
enabled = true
exclude_collections = ["users"]  # Hide sensitive collections
```

`exclude_collections` takes precedence when a collection appears in both lists.

## Security & Access Model

MCP operates with **full access** — collection-level and field-level access control
functions are not applied. This is by design: MCP is a machine-to-machine API surface
(equivalent to Lua's `overrideAccess = true`), gated by transport-level authentication:

- **stdio:** Access is controlled by who can run the process.
- **HTTP:** Access is controlled by the `api_key` setting. **Always set an API key
  when `http = true`.** Without one, the `/mcp` endpoint is completely unauthenticated —
  anyone who can reach the admin port gets full CRUD access to all collections.
  A startup warning is logged when HTTP is enabled without an API key.

To restrict which collections are visible, use `include_collections` / `exclude_collections`.

All MCP write operations (create, update, delete) are logged at `info` level for
audit purposes. Hooks still fire on all MCP writes (same lifecycle as admin/gRPC).

## Resources

The MCP server also exposes read-only resources:

| URI | Description |
|-----|-------------|
| `crap://schema/collections` | Full schema of all collections as JSON |
| `crap://schema/globals` | Full schema of all globals as JSON |
| `crap://config` | Current configuration (secrets sanitized: `auth.secret`, `email.smtp_pass`, `mcp.api_key`) |

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

### Response Format

`find_*` tools return a JSON object with `docs` and `pagination`:

```json
{
  "docs": [
    { "id": "abc123", "title": "Hello World", "created_at": "2026-01-15T09:00:00Z" }
  ],
  "pagination": {
    "totalDocs": 25,
    "limit": 10,
    "hasNextPage": true,
    "hasPrevPage": false,
    "totalPages": 3,
    "page": 1,
    "pageStart": 1,
    "nextPage": 2
  }
}
```

In cursor mode, `page`/`totalPages`/`pageStart`/`nextPage`/`prevPage` are replaced by `startCursor`/`endCursor`.

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

Supported operators: `equals`, `not_equals`, `greater_than`, `greater_than_equal`,
`less_than`, `less_than_equal`, `like`, `contains`, `in` (array), `not_in` (array),
`exists`, `not_exists`.

> **Note:** MCP uses shortened operator names (`greater_than_equal`, `less_than_equal`) compared to the gRPC/Lua API which uses `greater_than_or_equal` and `less_than_or_equal`.
