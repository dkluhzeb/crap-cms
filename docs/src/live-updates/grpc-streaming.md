# gRPC Subscribe RPC

The `Subscribe` RPC provides a server-streaming endpoint for real-time mutation events.

## Request

```protobuf
message SubscribeRequest {
  repeated string collections = 1;  // empty = all accessible
  repeated string globals = 2;      // empty = all accessible
  repeated string operations = 3;   // "create","update","delete","undelete","unpublish","restore" — empty = all
  string token = 4;                 // auth token from Login RPC
}
```

An unknown operation name is rejected with `INVALID_ARGUMENT`.

## Response Stream

```protobuf
message MutationEvent {
  uint64 sequence = 1;
  string timestamp = 2;
  MutationTarget target = 3;       // COLLECTION or GLOBAL
  MutationOperation operation = 4; // CREATE, UPDATE, DELETE, UNDELETE, UNPUBLISH or RESTORE
  string collection = 5;
  string document_id = 6;
  DataMap data = 7;
  string publisher = 8;       // node that published it; sequence is per publisher
}

enum MutationOperation {
  MUTATION_OPERATION_UNSPECIFIED = 0;
  MUTATION_OPERATION_CREATE = 1;
  MUTATION_OPERATION_UPDATE = 2;
  MUTATION_OPERATION_DELETE = 3;
  MUTATION_OPERATION_UNDELETE = 4;   // restored from the trash
  MUTATION_OPERATION_UNPUBLISH = 5;  // published document/global reverted to draft
  MUTATION_OPERATION_RESTORE = 6;    // version snapshot restored over the live one
}
```

The `data` payload is a `DataMap` (`map<string, FieldValue>` keyed by field
name); each `FieldValue` is a `oneof` over the typed value kinds
(`int_value`/`double_value`/`string_value`/`bool_value`/`struct_value`/`list_value`/`null_value`),
so numbers keep full precision (integers arrive as `int_value`, not a rounded
`double`). See [Type Safety](../grpc-api/type-safety.md) for the definitions.

Events deliberately do **not** identify the editing user — exposing editor
ids/emails to every subscriber would leak PII. Editor-based suppression or
transformation belongs in the server-side `live` filter and `before_broadcast`
hooks, whose contexts carry `edited_by` (see [Hooks](hooks.md)).

## Usage with grpcurl

```bash
# Subscribe to all collections
grpcurl -plaintext -d '{}' localhost:50051 crap.ContentAPI/Subscribe

# Subscribe to specific collections with auth
grpcurl -plaintext -d '{
  "collections": ["posts"],
  "operations": ["create", "update"],
  "token": "your-jwt-token"
}' localhost:50051 crap.ContentAPI/Subscribe
```

## Access Control

- Authentication via `token` field (same token as `Login` response)
- Access is resolved at subscribe time per content view — `read` (published),
  `draft` and `trash` — for each requested collection/global, and each event is
  delivered only to subscribers allowed the view it belongs to (a draft event
  needs `draft`, a soft-delete `trash`); row constraints and, in `full` mode,
  field-level access apply per subscriber. A subscriber that could see a
  document in the view it left (a publish, unpublish, status-changing restore,
  soft delete or undelete) but not in the view it moved into receives the
  event as a `DELETE` (a global leaving the published view as an `UPDATE`
  carrying the empty global) — and only when its `operations` include that
  removal operation. See
  [Access Control](overview.md#access-control).
- Collections/globals with no visible content view are silently excluded
- Returns `PERMISSION_DENIED` if no collections or globals are accessible
- Returns `UNAVAILABLE` if live updates are disabled in config

## Reconnection

If the stream is interrupted, clients should reconnect. Events missed during disconnection are not replayed. Use the `(publisher, sequence)` pair to detect gaps — `sequence` increases per publishing node.

## Connection Limits

The maximum number of concurrent Subscribe streams is controlled by `max_subscribe_connections` in `[live]` (default: 1000). When the limit is reached, new subscriptions receive `RESOURCE_EXHAUSTED` status. Set to `0` for unlimited.

(Note: `UNAVAILABLE` is returned for a different condition — live updates being disabled in config — not for hitting the connection limit.)

## Backpressure

The internal broadcast channel has a configurable capacity (default 1024). If a subscriber falls behind by more than `channel_capacity` events, it is **dropped** on its next read — the stream is closed (logged as a warning on the server) and the client must reconnect. Use the `(publisher, sequence)` pair to detect the gap. Earlier builds kept lagging subscribers alive with a warning, which silently dropped events; subscribers are now closed deterministically. Raise `channel_capacity` in `[live]` if legitimate subscribers are being dropped under bursty load.
