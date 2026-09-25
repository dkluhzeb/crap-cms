# Live Updates

Crap CMS supports real-time event streaming for mutation notifications. When documents are created, updated, or deleted, events are broadcast to connected subscribers.

Every write surface publishes: gRPC/REST, the admin UI, MCP, Lua CRUD in hooks,
and [job handlers](../jobs/overview.md) (whose Lua CRUD writes emit events by
default). With the Redis transport, writes performed by other processes — a
standalone [`crap-cms work`](../deployment/multi-server.md) worker or a stdio
MCP server — reach this server's subscribers too.

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

Set `enabled = false` to disable live updates entirely. gRPC Subscribe fails with `UNAVAILABLE`; the admin SSE endpoint still accepts the connection (`200`) but serves an empty stream that never emits events — so the admin UI degrades gracefully instead of erroring.

Connection limits protect against resource exhaustion. When the limit is reached, new SSE connections receive `503 Service Unavailable` and new gRPC Subscribe calls receive `RESOURCE_EXHAUSTED` status. (gRPC `UNAVAILABLE` is reserved for live updates being disabled, a different condition.) Existing connections are not affected.

### Subscriber lifecycle

Live-update subscribers (gRPC Subscribe or admin SSE) can be terminated by the server in three cases — all surface to the client as a closed stream and require a reconnect:

- **Send timeout (SEC-D)** — if forwarding an event to a specific subscriber takes longer than `subscriber_send_timeout_ms` (default 1000 ms), that subscriber is dropped. Healthy subscribers are unaffected.
- **Lag drop (SEC-D)** — if the broadcast channel overflows for a particular subscriber (it fell behind by more than `channel_capacity` events), that subscriber is dropped on its next read. Previously such subscribers were kept alive with a warning, which masked silent event loss; they are now closed deterministically.
- **User session revocation (SEC-E)** — when a user is locked or hard-deleted via the service layer, every active stream owned by that user is immediately torn down with `PermissionDenied`. Anonymous subscribers are unaffected.

## Event Delivery Modes

Each collection can control what data events carry:

- **`metadata`** (default) — events carry only metadata: sequence, timestamp, operation, collection, document_id (plus `self` on admin SSE). No document data is included. Metadata mode skips the per-subscriber `after_read` hooks and field-level read-access stripping on the event payload, because there is no payload to transform. The `before_broadcast` hook **still runs** (once per event, pre-dispatch) and the collection's `live` filter function still gates whether the event is broadcast at all. Clients re-fetch via `FindByID` if they need document data.

- **`full`** — events carry complete document data, processed through `after_read` hooks and field-level access stripping — the same data a `Find` or `FindByID` call by that subscriber would return. Opt-in per collection. Each subscriber's payload starts from the document **as stored** (as a `before_broadcast` hook left it), never from the document as returned to the user who made the change: a subscriber allowed a field the editor may not read receives it, and a field the subscriber may not read is stripped whatever the editor could read. Hidden fields never reach any subscriber. Localized fields carry the **default locale's** values (a subscriber picks no locale, so its payload is what its own read without a `locale` returns), whichever locale the change was written in — a subscriber working in another locale re-fetches the document with its `locale` on the event.

**Performance note:** In `full` mode, `after_read` hooks run once per event per subscriber. For collections with expensive hooks and many subscribers, use `metadata` mode and let clients re-fetch.

**Burst coalescing:** each subscriber's stream drains everything already
queued in one sweep and collapses it **latest-wins per document** before
processing — a burst of ten updates to one document costs one gate +
`after_read` pass and delivers one event carrying the newest state (with
that event's own sequence/operation). Collapsing never hides a move between
content views (see *Leaving a view* below): the surviving event remembers
where the document was before the burst, so a subscriber that could see it
there but not where it ended up still receives its removal — an unpublish
followed by a draft save arrives as one event that a published-only
subscriber receives as a `delete`. A draft save of a published document
describes its pending draft while the document itself stays published, so it
never stands for the document's position: it coalesces only with other draft
saves of that document, never with the document's own events — an update
followed by a draft save reaches a published-only subscriber as the update
(never as a removal), and a draft save followed by an unpublish still
announces the unpublish. A
subscriber that keeps up sees every event unchanged; coalescing only ever
touches events that were already queued behind it. Delivery granularity under load is deliberately
non-contractual (see the frozen-contracts internals doc): you always
receive the latest state, but intermediate events may collapse. This also
makes lag force-drops rare — a subscriber is disconnected only when even
the drained sweep cannot keep up with the broadcast buffer.

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

The filter function receives a typed `crap.LiveFilterContext` (`{ collection, operation, data, id, edited_by, options }`; `operation` is one of `"create"`, `"update"`, `"delete"`, `"undelete"`, `"unpublish"`, `"restore"` — the affected document's id is `ctx.id`, matching the other hook contexts; the serialized event payload calls the same value `document_id`) and returns `true` to broadcast or `false`/`nil` to suppress. Returning a table is a hook error (the event is not broadcast); any other type suppresses with a warning — the same boolean rule every other Lua gate follows.

`filter` may be a bare ref string **or** a `{ ref, options }` table — the options reach the filter as `ctx.options`, so one gate function can be reused across collections with different config:

```lua
live = { mode = "full", filter = { ref = "hooks.live.status_gate", options = { allow = { "published" } } } }
```

The `live = { ... }` table form is strict: `mode` must be `"full"` or `"metadata"` (the default), `filter` must be a valid hook ref (string or `{ ref, options }`), and any unknown key is a hard error at load time — a typo is not silently ignored.

## Access Control

> **Timing guarantee:** mutation events are published only after the
> originating transaction commits — on every surface, including
> hook-initiated (conn-mode) writes and `crap.transaction(fn)` blocks. A
> rolled-back write never emits an event.

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
- A **soft-delete** event needs `trash` (a subscriber that saw the document
  in its status view but may not see the trash is sent a removal — see
  *Leaving a view*); a hard-delete is gated by the view the
  document was last in — `trash` for a document that was in the trash (a purge
  of the trash, or a forced permanent delete of a trashed document), its
  `read`/`draft` view otherwise.

The event carries this view metadata independent of `mode`, so gating holds even
in `metadata` mode and for delete events, where the payload is empty. The views
are independent: a draft-only reviewer (granted `draft`, denied `read`) receives
draft events but not published ones.

**Leaving a view.** A write can move a document from one content view to
another: publishing a draft (draft → published), unpublishing it or restoring
a draft version over it (published → draft), restoring a published version
over a draft, soft-deleting it (its status view → trash) and undeleting it
(trash → its status view). The event is gated by the view the document moved
into, so a subscriber that can see that view receives the event itself. A
subscriber that could see the document where it was (that view, row
constraint included, admits the row) but cannot see where it is now is told
it is gone instead:

- a **collection document** arrives as a **`delete`** (no data, in either
  mode) — it left the subscriber's view exactly as a deleted one does;
- a **global** that leaves the published view (an unpublish, or a draft
  version restored over it) arrives as an **`update`** carrying the empty
  global a non-draft read now returns (in `full` mode: no field content,
  `_status = "draft"`). A global is always there to read in its draft view,
  so publishing one announces no removal; globals have no trash.

Whether the subscriber could see the document where it was is judged
against the document **as it was there**: a publish that also changed the
content, or a version restore, is judged in the view it left against the old
content, not the new — so a subscriber whose constraint the old row satisfied
is told of the removal even when the new content no longer matches, and one
whose constraint only the new content satisfies (it never saw the row) learns
nothing, not even its id. Coalesced bursts are judged against the content the
burst started from.

So a published-only subscriber is told when a document is unpublished or
trashed; a draft-view subscriber without `trash` access when a draft is
trashed or published; a trash-only subscriber when a document is undeleted.
A subscriber that could not see the document where it was learns nothing — a
draft that is trashed or unpublished again never reaches a published-only
subscriber, so it never learns the draft exists. On the gRPC stream, a
subscription scoped to `operations` receives the removal only when it asked
for `delete` (collections) or `update` (globals).

In a multi-server deployment, nodes that predate this announce only a move
out of the published view (to published-only subscribers); an event such a
node publishes is read the same way by an upgraded one. Once every node runs
the current version, every move is announced.

Row-level constraints use in-memory evaluation of the same filters that `Find` uses as SQL WHERE conditions. For example, if a user's access returns `{ owner = ctx.user.id }`, only events where `owner` matches are delivered.

**What a constraint is judged against.** Every event carries, next to what it
delivers, a snapshot of the stored row it concerns — used only to judge
subscribers' row constraints and **never delivered** to any subscriber, in
either mode, on either stream. The snapshot is the row as stored, the way the
SQL read path filters it: hidden and read-denied fields included, plus `id`
and the timestamps. So:

- a `metadata`-mode event reaches a constrained subscriber exactly when the
  changed row satisfies the constraint — still as metadata only;
- a **delete** (soft or hard, single or bulk, on every surface) reaches a
  constrained subscriber exactly when the deleted row satisfied it. A hard
  delete is judged against the row as it was just before removal, a soft
  delete against the row as it now sits in the trash (gated by the `trash`
  view);
- a `full`-mode event is judged against the stored row, not its delivered
  payload — a `before_broadcast` hook that reshapes `data` changes neither
  which subscribers receive it nor lets a payload claim a row it isn't.

Within the snapshot, a field held as `null` matches as a SQL `NULL` does (only
`not_exists`). An event that carries no snapshot — one published by a node that
predates it during a rolling upgrade, or one whose snapshot was dropped to fit
the Redis transport's payload cap — cannot be judged, so a constrained
subscriber never receives it (fail-closed); unconstrained subscribers receive
it as usual.

**Purges publish their deletes.** Every permanent delete publishes a delete
event — a purge of already-trashed documents included: the scheduled
retention purge, "Empty trash" in the admin, `crap-cms trash purge` /
`trash empty`, and a `delete_many` with `trash = true` that opts into events.
Each purged document's event is gated by the `trash` view and judged against
the row as it sat in the trash, and it is published only once the purge has
committed. The CLI publishes on the configured transport: with `transport =
"redis"` the purge reaches `serve`'s subscribers like any other process's
writes; with the in-memory transport a CLI process has no subscribers of its
own. A large purge publishes one event per document — burst coalescing
collapses only repeated events for the *same* document, so a purge larger
than `channel_capacity` can make a slow subscriber lag and be disconnected
(see *Subscriber lifecycle*); it reconnects and refetches, as after any
missed events.

**CLI writes publish like the server's.** `crap-cms trash restore` publishes
an undelete event and `crap-cms user delete` a delete event, on the same
configured transport as the CLI purges.

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
> every node is upgraded. The same holds for the stored-row snapshot: an event
> from a node that predates it reaches only subscribers without a row
> constraint.

## Event Structure

| Field | Description | metadata | full |
|-------|-------------|:---:|:---:|
| `sequence` | Sequence number, monotonic per `publisher` | ✅ | ✅ |
| `publisher` | Id of the server process that published the event; detect gaps on `(publisher, sequence)` | ✅ | ✅ |
| `timestamp` | ISO 8601 timestamp | ✅ | ✅ |
| `target` | `"collection"` or `"global"` | ✅ | ✅ |
| `operation` | `"create"`, `"update"`, `"delete"`, `"undelete"`, `"unpublish"`, `"restore"` | ✅ | ✅ |
| `collection` | Collection or global slug | ✅ | ✅ |
| `document_id` | Document ID | ✅ | ✅ |
| `data` | Document fields, stripped and hook-processed for the subscriber | empty | ✅ |
| `self` | Whether the subscriber made the change (admin SSE only) | ✅ | ✅ |

The stored-row snapshot used for row-constraint gating is not part of the
delivered event.

Events never identify the editing user to subscribers — exposing editor
ids/emails would leak PII. The server-side `live` filter and
`before_broadcast` contexts carry `edited_by` for editor-based logic.

## Event Pipeline

```
Transaction:
  before-hooks → DB operation → after-hooks → commit

After commit:
  -> publish_event()           (the event carries the stored document)
       1. live setting check (enabled/disabled/function)
       2. before_broadcast hooks (can modify/suppress)
       3. EventBus.publish()   (metadata mode: the document is dropped here)
            -> Per subscriber:
                 a. content-view access (cached read/draft/trash)
                 b. row-level constraints (cached, in-memory, judged
                    against the event's stored-row snapshot)
                 c. mode:
                    metadata → deliver metadata only
                    full → field strip (the subscriber's access) → hidden
                           strip → after_read hooks → deliver
```

The `live` filter and the `before_broadcast` hooks see the document as stored
— read-shaped, with hidden and read-denied fields, in both modes — like
`after_change` does; they run once per event, before any subscriber's strip.

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
