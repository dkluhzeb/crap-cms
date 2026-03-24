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
  string target = 3;          // "collection" or "global"
  string operation = 4;       // "create", "update", "delete"
  string collection = 5;
  string document_id = 6;
  google.protobuf.Struct data = 7;
}
```

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

The maximum number of concurrent Subscribe streams is controlled by `max_subscribe_connections` in `[live]` (default: 1000). When the limit is reached, new subscriptions receive `UNAVAILABLE` status. Set to `0` for unlimited.

## Backpressure

The internal broadcast channel has a configurable capacity (default 1024). If a subscriber falls behind, events are dropped and the stream continues from the latest event (logged as a warning on the server).
