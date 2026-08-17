# gRPC Subscribe RPC

The `Subscribe` RPC provides a server-streaming endpoint for real-time mutation events.

## Request

```protobuf
message SubscribeRequest {
  repeated string collections = 1;  // empty = all accessible
  repeated string globals = 2;      // empty = all accessible
  repeated string operations = 3;   // "create","update","delete" — empty = all
  string token = 4;                 // auth token from Login RPC
}
```

## Response Stream

```protobuf
message MutationEvent {
  uint64 sequence = 1;
  string timestamp = 2;
  MutationTarget target = 3;       // COLLECTION or GLOBAL
  MutationOperation operation = 4; // CREATE, UPDATE, or DELETE
  string collection = 5;
  string document_id = 6;
  DataMap data = 7;
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
- Read access is checked at subscribe time for each requested collection/global
- Collections/globals without read access are silently excluded
- Returns `PERMISSION_DENIED` if no collections or globals are accessible
- Returns `UNAVAILABLE` if live updates are disabled in config

## Reconnection

If the stream is interrupted, clients should reconnect. Events missed during disconnection are not replayed. Use the `sequence` field to detect gaps.

## Connection Limits

The maximum number of concurrent Subscribe streams is controlled by `max_subscribe_connections` in `[live]` (default: 1000). When the limit is reached, new subscriptions receive `RESOURCE_EXHAUSTED` status. Set to `0` for unlimited.

(Note: `UNAVAILABLE` is returned for a different condition — live updates being disabled in config — not for hitting the connection limit.)

## Backpressure

The internal broadcast channel has a configurable capacity (default 1024). If a subscriber falls behind by more than `channel_capacity` events, it is **dropped** on its next read — the stream is closed (logged as a warning on the server) and the client must reconnect. Use the `sequence` field to detect the gap. Earlier builds kept lagging subscribers alive with a warning, which silently dropped events; subscribers are now closed deterministically. Raise `channel_capacity` in `[live]` if legitimate subscribers are being dropped under bursty load.
