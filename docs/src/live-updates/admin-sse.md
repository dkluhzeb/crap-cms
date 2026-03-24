# Admin SSE Endpoint

The admin UI includes a Server-Sent Events (SSE) endpoint for real-time mutation notifications.

## Endpoint

```
GET /admin/events
```

Protected by admin auth middleware (requires valid session cookie).

## Event Format

Events are sent with event type `mutation`:

```
event: mutation
id: 42
data: {"sequence":42,"timestamp":"2024-01-15T10:30:00Z","target":"collection","operation":"create","collection":"posts","document_id":"abc123"}
```

The `data` payload is JSON with the same fields as the gRPC `MutationEvent` (excluding the full document `data` for efficiency — admin SSE only sends metadata).

## Admin UI Integration

The admin UI automatically connects to the SSE endpoint on all authenticated pages. When a mutation event is received, a toast notification is shown via the `<crap-toast>` component.

The SSE connection:
- Auto-reconnects on disconnection (native `EventSource` behavior)
- Sends keepalive pings every 30 seconds
- Only activates on pages with the admin layout

## Access Control

Same as gRPC Subscribe: read access is checked at connection time per collection/global. Events for inaccessible collections are filtered out.

> **Note:** Access control is **snapshotted at subscribe time**. If a user's permissions change after they subscribe to the SSE stream (e.g., their role is updated or access rules are modified), they will continue receiving events based on the original permissions until the SSE connection is closed. To force a re-evaluation, the client must reconnect.

## Custom Integration

If you override the admin templates, the SSE listener is in `static/components/live-events.js`. You can customize or replace it by placing your own `static/components/live-events.js` in your config dir's `static/` folder.
