# Live Updates

Crap CMS supports real-time event streaming for mutation notifications. When documents are created, updated, or deleted, events are broadcast to connected subscribers.

## Technology

- **gRPC Server Streaming** (`Subscribe` RPC) for API consumers
- **SSE** (`GET /admin/events`) for the admin UI
- **Transport**: pluggable. Default is in-process (`tokio::sync::broadcast`); behind `--features redis` you can switch to Redis pub/sub for cross-node fanout — see [Multi-Server Deployment](../deployment/multi-server.md).

## Configuration

In `crap.toml`:

```toml
[live]
enabled = true              # default: true
transport = "memory"        # default: "memory" — in-process; set to "redis" for multi-node fanout
channel_capacity = 1024     # default: 1024
# max_sse_connections = 1000        # max concurrent SSE connections (0 = unlimited)
# max_subscribe_connections = 1000  # max concurrent gRPC Subscribe streams (0 = unlimited)
# subscriber_send_timeout_ms = 1000 # drop slow subscribers after this many ms (default: 1000)
```

`transport = "redis"` uses the same Redis URL as `[cache] redis_url` (single source of truth). When the binary isn't built with `--features redis`, selecting `"redis"` aborts startup with a clear error.

Set `enabled = false` to disable live updates entirely. Both SSE and gRPC Subscribe will be unavailable.

Connection limits protect against resource exhaustion. When the limit is reached, new SSE connections receive `503 Service Unavailable` and new gRPC Subscribe calls receive `RESOURCE_EXHAUSTED` status. (gRPC `UNAVAILABLE` is reserved for live updates being disabled, a different condition.) Existing connections are not affected.

### Subscriber lifecycle

Live-update subscribers (gRPC Subscribe or admin SSE) can be terminated by the server in three cases — all surface to the client as a closed stream and require a reconnect:

- **Send timeout (SEC-D)** — if forwarding an event to a specific subscriber takes longer than `subscriber_send_timeout_ms` (default 1000 ms), that subscriber is dropped. Healthy subscribers are unaffected.
- **Lag drop (SEC-D)** — if the broadcast channel overflows for a particular subscriber (it fell behind by more than `channel_capacity` events), that subscriber is dropped on its next read. Previously such subscribers were kept alive with a warning, which masked silent event loss; they are now closed deterministically.
- **User session revocation (SEC-E)** — when a user is locked or hard-deleted via the service layer, every active stream owned by that user is immediately torn down with `PermissionDenied`. Anonymous subscribers are unaffected.

## Event Delivery Modes

Each collection can control what data events carry:

- **`metadata`** (default) — events carry only metadata: sequence, timestamp, operation, collection, document_id (plus `self` on admin SSE). No document data is included. Metadata mode skips the per-subscriber `after_read` hooks and field-level read-access stripping on the event payload, because there is no payload to transform. The `before_broadcast` hook **still runs** (once per event, pre-dispatch) and the collection's `live` filter function still gates whether the event is broadcast at all. Clients re-fetch via `FindByID` if they need document data.

- **`full`** — events carry complete document data, processed through `after_read` hooks and field-level access stripping — the same data a `Find` or `FindByID` call would return. Opt-in per collection.

**Performance note:** In `full` mode, `after_read` hooks run once per event per subscriber. For collections with expensive hooks and many subscribers, use `metadata` mode and let clients re-fetch.

## Per-Collection Control

```lua
-- Broadcast all events in metadata mode (default)
crap.collections.define("posts", { ... })

-- Disable broadcasting entirely
crap.collections.define("audit_log", {
    live = false,
    ...
})

-- Full data mode: events include document data with after_read hooks
crap.collections.define("posts", {
    live = { mode = "full" },
    ...
})

-- Full data mode with a Lua filter function
crap.collections.define("posts", {
    live = { mode = "full", filter = "hooks.posts.should_broadcast" },
    ...
})

-- Lua function decides per-event (metadata mode)
crap.collections.define("posts", {
    live = "hooks.posts.should_broadcast",
    ...
})
```

The filter function receives a typed `crap.LiveFilterContext` (`{ collection, operation, data, id, edited_by, options }` — the affected document's id is `ctx.id`, matching the other hook contexts; the serialized event payload calls the same value `document_id`) and returns `true` to broadcast or `false`/`nil` to suppress.

`filter` may be a bare ref string **or** a `{ ref, options }` table — the options reach the filter as `ctx.options`, so one gate function can be reused across collections with different config:

```lua
live = { mode = "full", filter = { ref = "hooks.live.status_gate", options = { allow = { "published" } } } }
```

The `live = { ... }` table form is strict: `mode` must be `"full"` or `"metadata"` (the default), `filter` must be a valid hook ref (string or `{ ref, options }`), and any unknown key is a hard error at load time — a typo is not silently ignored.

## Access Control

Event streams enforce the same access rules as normal read operations:

| Layer | metadata | full | Description |
|-------|:---:|:---:|-------------|
| Collection-level access | ✅ | ✅ | Only collections with at least one visible content view |
| Content-view access | ✅ | ✅ | Each event gated by the view it belongs to (`read`/`draft`/`trash`) |
| Row-level constraints | ✅ | ✅ | Constraint filters evaluated in-memory per event |
| `after_read` hooks | — | ✅ | Data transformed per subscriber (same as Find) |
| Field-level access | — | ✅ | Denied fields stripped per subscriber |
| `before_broadcast` hooks | ✅ | ✅ | Can modify/suppress events before delivery |

**Content-view gating.** Each mutation event belongs to exactly one content
view, and is delivered only to subscribers allowed to see that view — the same
independent `read` (published) / `draft` / `trash` keys that gate normal reads:

- A **published** document's create/update event needs `read`.
- A **draft** document's create/update event needs `draft` (default: falls back
  to `update`). A `read`-only subscriber never sees draft events.
- A **soft-delete** event needs `trash`; a hard-delete is gated by the view the
  document was last in.

The event carries this view metadata independent of `mode`, so gating holds even
in `metadata` mode and for delete events, where the payload is empty. The views
are independent: a draft-only reviewer (granted `draft`, denied `read`) receives
draft events but not published ones.

Row-level constraints use in-memory evaluation of the same filters that `Find` uses as SQL WHERE conditions. For example, if a user's access returns `{ owner = ctx.user.id }`, only events where `owner` matches are delivered. (An event whose payload is empty — `metadata` mode, or any delete — cannot satisfy a non-empty row constraint, so a constrained subscriber is fail-closed for those events.)

Access is snapshotted at subscribe time and re-resolved only on reconnect.

> **Revoking live access requires a session-version bump.** The server tears
> down a stream (forcing the re-resolve) only when the subscriber's session is
> invalidated — lock, hard-delete, logout, or password reset, which all bump
> `_session_version` (see SEC-E above). A permission change made *purely* by
> editing data the access function reads — e.g. removing a role document or a
> group membership without touching the subscriber's own session — does **not**
> tear down an already-open stream, so the stale snapshot keeps delivering
> events for the now-revoked view until the client reconnects. To revoke a
> user's live access immediately, bump their session version (e.g. `lock` then
> unlock, or any session-invalidating action). Same-process and multi-node
> behave identically here: there is no "access changed" signal, only
> "session invalidated."

> **Multi-node rolling upgrades.** Content-view gating relies on view metadata
> the publisher attaches to each event. An event that arrives **without** it —
> e.g. published by a node running a version that predates per-view gating, over
> a shared Redis transport during a rolling upgrade — cannot be safely gated, so
> consumers **drop it** (fail-closed) rather than guess a view. Live updates are
> best-effort (clients refetch on reconnect), so this only means a brief gap in
> events originating from not-yet-upgraded nodes; gating is fully effective once
> every node is upgraded.

## Event Structure

| Field | Description | metadata | full |
|-------|-------------|:---:|:---:|
| `sequence` | Monotonic sequence number | ✅ | ✅ |
| `timestamp` | ISO 8601 timestamp | ✅ | ✅ |
| `target` | `"collection"` or `"global"` | ✅ | ✅ |
| `operation` | `"create"`, `"update"`, `"delete"` | ✅ | ✅ |
| `collection` | Collection or global slug | ✅ | ✅ |
| `document_id` | Document ID | ✅ | ✅ |
| `data` | Document fields (hook-processed) | empty | ✅ |
| `self` | Whether the subscriber made the change (admin SSE only) | ✅ | ✅ |

Events never identify the editing user to subscribers — exposing editor
ids/emails would leak PII. The server-side `live` filter and
`before_broadcast` contexts carry `edited_by` for editor-based logic.

## Event Pipeline

```
Transaction:
  before-hooks → DB operation → after-hooks → commit

After commit:
  -> publish_event()
       1. live setting check (enabled/disabled/function)
       2. before_broadcast hooks (can modify/suppress)
       3. EventBus.publish()
            -> Per subscriber:
                 a. content-view access (cached read/draft/trash)
                 b. row-level constraints (cached, in-memory)
                 c. mode:
                    metadata → deliver metadata only
                    full → after_read hooks → field strip → deliver
```

The content-view access and row constraints (a, b) are resolved **once at
subscribe time** and reused. The field strip in (c) is **not** cached — each
event re-runs the data-aware field-read rules against that event's own document
(`ctx.data` / `ctx.document`), so a rule that depends on document values gates
each event individually. The admin SSE stream and the gRPC `Subscribe` stream
share one implementation of this gate, so they cannot drift.

## Limitations

- Events are **ephemeral** — missed events are not replayed
- Access is **snapshotted at subscribe time** — permission changes require reconnect
- No field-level subscription filters
- No event persistence or replay
- `before_broadcast` hooks have no CRUD access (fires after commit)
- In `full` mode, `after_read` hooks run per subscriber — expensive hooks may impact performance at scale
