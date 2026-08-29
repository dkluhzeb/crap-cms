<!-- GENERATED FILE — do not edit. Regenerate with `cargo xtask gen-wire-doc`. -->

# Operation options reference

The wire options of every CRUD operation, generated from the
single-source wire model (`service::op::wire`) — the same model that
renders the MCP tool schemas and that the wire-parity test checks
`proto/content.proto` and `types/crap.lua` against.

Conventions:

- **Surfaces** — where the option exists. Routing (the collection
slug in the gRPC message / MCP tool name / Lua argument), Lua's
`override_access`, and Lua's positional arguments (`id`, `data`,
`documents`, `version_id`) are structural per surface and not
listed as options.
- **where filter** — the canonical filter grammar: an object on
MCP/Lua, a JSON string on gRPC.
- **field data** — the document payload, shaped by the collection's
field definitions.
- `unpublish` has no gRPC RPC of its own — gRPC spells it as the
`unpublish` flag on `update`.

## Collection operations

### `find`

| Field | Type | Required | Surfaces | Description |
|-------|------|----------|----------|-------------|
| `where` | where filter |  | gRPC, MCP, Lua | Filter conditions. Keys are field names, values are filter objects (e.g. {"equals": "value"}, {"contains": "text"}, {"greater_than": 5}) |
| `order_by` | string |  | gRPC, MCP, Lua | Sort field (prefix with - for descending) |
| `limit` | integer |  | gRPC, MCP, Lua | Max results per page |
| `page` | integer |  | gRPC, MCP, Lua | Page number (1-indexed, page mode only) |
| `offset` | integer |  | Lua | Number of results to skip |
| `after_cursor` | string |  | gRPC, MCP, Lua | Forward cursor (cursor mode only, mutually exclusive with page and before_cursor) |
| `before_cursor` | string |  | gRPC, MCP, Lua | Backward cursor (cursor mode only, mutually exclusive with page and after_cursor) |
| `depth` | integer |  | gRPC, MCP, Lua | Relationship population depth |
| `search` | string |  | gRPC, MCP, Lua | Full-text search query |
| `locale` | locale (string) |  | gRPC, MCP, Lua | Locale code (e.g. 'en', 'de') or 'all' for all locales |
| `draft` | boolean |  | gRPC, MCP, Lua | When true, include draft documents (published + draft union) |
| `trash` | boolean |  | gRPC, MCP, Lua | When true, return only soft-deleted documents (trash view) |
| `select` | string[] |  | gRPC, MCP, Lua | Field names to return (projection); omit for all fields |

### `find_by_id`

| Field | Type | Required | Surfaces | Description |
|-------|------|----------|----------|-------------|
| `id` | id (string) | yes | gRPC, MCP, Lua |  |
| `depth` | integer |  | gRPC, MCP, Lua | Relationship population depth |
| `locale` | locale (string) |  | gRPC, MCP, Lua | Locale code (e.g. 'en', 'de') or 'all' for all locales |
| `draft` | boolean |  | gRPC, MCP, Lua | When true, overlay the latest draft version (draft view) |
| `trash` | boolean |  | gRPC, MCP, Lua | When true, look up among soft-deleted documents (trash view) |
| `select` | string[] |  | gRPC, MCP, Lua | Field names to return (projection); omit for all fields |

### `count`

| Field | Type | Required | Surfaces | Description |
|-------|------|----------|----------|-------------|
| `where` | where filter |  | gRPC, MCP, Lua | Filter conditions. Keys are field names, values are filter objects (e.g. {"equals": "value"}, {"contains": "text"}, {"greater_than": 5}) |
| `search` | string |  | gRPC, MCP, Lua | Full-text search query |
| `locale` | locale (string) |  | gRPC, MCP, Lua | Locale code (e.g. 'en', 'de') or 'all' for all locales |
| `draft` | boolean |  | gRPC, MCP, Lua | When true, include draft documents in the count (published + draft union) |
| `trash` | boolean |  | gRPC, MCP, Lua | When true, count only soft-deleted documents (trash view) |

### `create`

| Field | Type | Required | Surfaces | Description |
|-------|------|----------|----------|-------------|
| `data` | field data (top-level) | yes | gRPC, MCP, Lua |  |
| `locale` | locale (string) |  | gRPC, MCP, Lua | Locale code (e.g. 'en', 'de') for localized fields |
| `draft` | boolean |  | gRPC, MCP, Lua | Write as a draft version (default: false) |
| `hooks` | boolean |  | Lua | Run per-document lifecycle hooks (default: true) |
| `events` | boolean |  | gRPC, MCP, Lua | Emit a live-update event for this change (default: true) |

### `update`

| Field | Type | Required | Surfaces | Description |
|-------|------|----------|----------|-------------|
| `id` | id (string) | yes | gRPC, MCP, Lua |  |
| `data` | field data (top-level) | yes | gRPC, MCP, Lua |  |
| `locale` | locale (string) |  | gRPC, MCP, Lua | Locale code (e.g. 'en', 'de') for localized fields |
| `draft` | boolean |  | gRPC, MCP, Lua | Write as a draft version (default: false) |
| `hooks` | boolean |  | Lua | Run per-document lifecycle hooks (default: true) |
| `unpublish` | boolean |  | gRPC, Lua | Transition a published document back to draft without changing field data |
| `events` | boolean |  | gRPC, MCP, Lua | Emit a live-update event for this change (default: true) |

### `validate`

| Field | Type | Required | Surfaces | Description |
|-------|------|----------|----------|-------------|
| `id` | id (string) |  | gRPC, MCP, Lua | Document ID — when set, validates as an update (excludes this row from unique checks) |
| `data` | field data (top-level) | yes | gRPC, MCP, Lua |  |
| `locale` | locale (string) |  | gRPC, MCP, Lua | Locale code (e.g. 'en', 'de') for localized fields |
| `draft` | boolean |  | gRPC, MCP, Lua | Validate as a draft version (default: false) |

### `delete`

| Field | Type | Required | Surfaces | Description |
|-------|------|----------|----------|-------------|
| `id` | id (string) | yes | gRPC, MCP, Lua |  |
| `force_hard_delete` | boolean |  | gRPC, MCP, Lua | Bypass soft-delete and remove the row permanently (default: false) |
| `hooks` | boolean |  | Lua | Run per-document lifecycle hooks (default: true) |
| `events` | boolean |  | gRPC, MCP, Lua | Emit a live-update event for this change (default: true) |

### `undelete`

| Field | Type | Required | Surfaces | Description |
|-------|------|----------|----------|-------------|
| `id` | id (string) | yes | gRPC, MCP, Lua |  |
| `events` | boolean |  | gRPC, MCP, Lua | Emit a live-update event for this change (default: true) |

### `unpublish`

| Field | Type | Required | Surfaces | Description |
|-------|------|----------|----------|-------------|
| `id` | id (string) | yes | gRPC, MCP, Lua |  |
| `hooks` | boolean |  | Lua | Run per-document lifecycle hooks (default: true) |
| `events` | boolean |  | gRPC, MCP, Lua | Emit a live-update event for this change (default: true) |

### `create_many`

| Field | Type | Required | Surfaces | Description |
|-------|------|----------|----------|-------------|
| `documents` | field data (`documents` array) | yes | gRPC, MCP, Lua | Array of documents to create |
| `locale` | locale (string) |  | gRPC, MCP, Lua | Locale code (e.g. 'en', 'de') for localized fields |
| `draft` | boolean |  | gRPC, MCP, Lua | Create documents as drafts (default: false) |
| `hooks` | boolean |  | gRPC, MCP, Lua | Run per-document lifecycle hooks (default: true) |
| `events` | boolean |  | gRPC, MCP, Lua | Emit a live-update event per created document (default: false — bulk ops are quiet) |

### `update_many`

| Field | Type | Required | Surfaces | Description |
|-------|------|----------|----------|-------------|
| `where` | where filter |  | gRPC, MCP, Lua | Filter conditions. Keys are field names, values are filter objects (e.g. {"equals": "value"}, {"contains": "text"}, {"greater_than": 5}) |
| `data` | field data (`data` object) | yes | gRPC, MCP, Lua | Field values to set on all matching documents |
| `hooks` | boolean |  | gRPC, MCP, Lua | Run per-document lifecycle hooks (default: true) |
| `draft` | boolean |  | gRPC, MCP, Lua | Target draft versions (default: false) |
| `locale` | locale (string) |  | gRPC, MCP, Lua | Locale code (e.g. 'en', 'de') for localized fields |
| `events` | boolean |  | gRPC, MCP, Lua | Emit a live-update event per modified document (default: false — bulk ops are quiet) |

### `delete_many`

| Field | Type | Required | Surfaces | Description |
|-------|------|----------|----------|-------------|
| `where` | where filter |  | gRPC, MCP, Lua | Filter conditions. Keys are field names, values are filter objects (e.g. {"equals": "value"}, {"contains": "text"}). Omit to match all documents. |
| `hooks` | boolean |  | gRPC, MCP, Lua | Run per-document lifecycle hooks (default: true) |
| `locale` | locale (string) |  | Lua | Locale code. Validated but not used for matching (delete_many spans locales) |
| `force_hard_delete` | boolean |  | gRPC, MCP, Lua | Force hard delete even on soft-delete collections (default: false) |
| `trash` | boolean |  | Lua | Target already-trashed documents and permanently remove them (empty the trash) |
| `events` | boolean |  | gRPC, MCP, Lua | Emit a live-update event per deleted document (default: false — bulk ops are quiet) |

### `list_versions`

| Field | Type | Required | Surfaces | Description |
|-------|------|----------|----------|-------------|
| `id` | id (string) | yes | gRPC, MCP, Lua | Document ID to list versions for |
| `limit` | integer |  | gRPC, MCP, Lua | Max versions to return |
| `offset` | integer |  | gRPC, MCP, Lua | Number of versions to skip |

### `restore_version`

| Field | Type | Required | Surfaces | Description |
|-------|------|----------|----------|-------------|
| `id` (gRPC: `document_id`) | id (string) | yes | gRPC, MCP, Lua | Document ID to restore |
| `version_id` | string | yes | gRPC, MCP, Lua | Version snapshot ID to restore from |

## Global operations

### `get_global`

| Field | Type | Required | Surfaces | Description |
|-------|------|----------|----------|-------------|
| `locale` | locale (string) |  | gRPC, MCP, Lua | Locale code (e.g. 'en', 'de') for localized fields |
| `draft` | boolean |  | gRPC, MCP, Lua | Read unpublished (draft) content (default: false) |

### `update_global`

| Field | Type | Required | Surfaces | Description |
|-------|------|----------|----------|-------------|
| `data` | field data (top-level) | yes | gRPC, MCP, Lua |  |
| `locale` | locale (string) |  | gRPC, MCP, Lua | Locale code (e.g. 'en', 'de') for localized fields |
| `draft` | boolean |  | gRPC, MCP, Lua | Write as a draft version (default: false) |
| `hooks` | boolean |  | Lua | Run per-document lifecycle hooks (default: true) |
| `events` | boolean |  | gRPC, MCP, Lua | Emit a live-update event for this change (default: true) |

### `validate_global`

| Field | Type | Required | Surfaces | Description |
|-------|------|----------|----------|-------------|
| `data` | field data (top-level) | yes | gRPC, MCP, Lua |  |
| `locale` | locale (string) |  | gRPC, MCP, Lua | Locale code (e.g. 'en', 'de') for localized fields |
| `draft` | boolean |  | gRPC, MCP, Lua | Validate as a draft version (default: false) |

