# Live Updates

Crap CMS supports real-time event streaming for mutation notifications. When documents are created, updated, or deleted, events are broadcast to connected subscribers.

## Technology

- **gRPC Server Streaming** (`Subscribe` RPC) for API consumers
- **SSE** (`GET /admin/events`) for the admin UI
- **Internal bus**: `tokio::sync::broadcast` channel

## Configuration

In `crap.toml`:

```toml
[live]
enabled = true           # default: true
channel_capacity = 1024  # default: 1024
```

Set `enabled = false` to disable live updates entirely. Both SSE and gRPC Subscribe will be unavailable.

## Per-Collection Control

Each collection (and global) can control whether it emits events via the `live` field:

```lua
-- Broadcast all events (default when absent)
crap.collections.define("posts", { ... })

-- Disable broadcasting entirely
crap.collections.define("audit_log", {
    live = false,
    ...
})

-- Dynamic: Lua function decides per-event
crap.collections.define("posts", {
    live = "hooks.posts.should_broadcast",
    ...
})
```

The function receives `{ collection, operation, data }` and returns `true` to broadcast or `false`/`nil` to suppress.

## Event Structure

Each event contains:

| Field | Description |
|-------|-------------|
| `sequence` | Monotonic sequence number |
| `timestamp` | ISO 8601 timestamp |
| `target` | `"collection"` or `"global"` |
| `operation` | `"create"`, `"update"`, or `"delete"` |
| `collection` | Collection or global slug |
| `document_id` | Document ID |
| `data` | Full document fields (empty for delete) |

## Event Pipeline

```
Transaction:
  before-hooks → DB operation → after-hooks → commit

After commit:
  -> publish_event()
       1. live setting check
       2. before_broadcast hooks
       3. EventBus.publish()
            -> gRPC Subscribe stream
            -> Admin SSE stream
```

## Limitations (V1)

- Events are **ephemeral** — missed events are not replayed
- Access is **snapshotted at subscribe time** — permission changes require reconnect
- No field-level subscription filters
- No event persistence or replay
- `before_broadcast` hooks have no CRUD access (fires after commit)
