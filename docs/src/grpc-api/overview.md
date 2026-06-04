# gRPC API

Crap CMS exposes a gRPC API via Tonic for programmatic access to all content operations.

## Service

```protobuf
service ContentAPI {
  rpc Find (FindRequest) returns (FindResponse);
  rpc FindByID (FindByIDRequest) returns (FindByIDResponse);
  rpc Create (CreateRequest) returns (CreateResponse);
  rpc CreateMany (CreateManyRequest) returns (CreateManyResponse);
  rpc Update (UpdateRequest) returns (UpdateResponse);
  rpc Delete (DeleteRequest) returns (DeleteResponse);
  rpc Undelete (UndeleteRequest) returns (UndeleteResponse);
  rpc Count (CountRequest) returns (CountResponse);
  rpc UpdateMany (UpdateManyRequest) returns (UpdateManyResponse);
  rpc DeleteMany (DeleteManyRequest) returns (DeleteManyResponse);
  rpc Validate (ValidateRequest) returns (ValidateResponse);
  rpc ValidateGlobal (ValidateGlobalRequest) returns (ValidateResponse);
  rpc GetGlobal (GetGlobalRequest) returns (GetGlobalResponse);
  rpc UpdateGlobal (UpdateGlobalRequest) returns (UpdateGlobalResponse);
  rpc Login (LoginRequest) returns (LoginResponse);
  rpc Me (MeRequest) returns (MeResponse);
  rpc ForgotPassword (ForgotPasswordRequest) returns (ForgotPasswordResponse);
  rpc ResetPassword (ResetPasswordRequest) returns (ResetPasswordResponse);
  rpc VerifyEmail (VerifyEmailRequest) returns (VerifyEmailResponse);
  rpc LockAccount (AccountActionRequest) returns (AccountActionResponse);
  rpc UnlockAccount (AccountActionRequest) returns (AccountActionResponse);
  rpc VerifyAccount (AccountActionRequest) returns (AccountActionResponse);
  rpc UnverifyAccount (AccountActionRequest) returns (AccountActionResponse);
  rpc ListCollections (ListCollectionsRequest) returns (ListCollectionsResponse);
  rpc DescribeCollection (DescribeCollectionRequest) returns (DescribeCollectionResponse);
  rpc Subscribe (SubscribeRequest) returns (stream MutationEvent);
  rpc ListVersions (ListVersionsRequest) returns (ListVersionsResponse);
  rpc RestoreVersion (RestoreVersionRequest) returns (RestoreVersionResponse);
  rpc ListJobs (ListJobsRequest) returns (ListJobsResponse);
  rpc TriggerJob (TriggerJobRequest) returns (TriggerJobResponse);
  rpc GetJobRun (GetJobRunRequest) returns (GetJobRunResponse);
  rpc ListJobRuns (ListJobRunsRequest) returns (ListJobRunsResponse);
}
```

## Port

Default: `50051` (configurable via `[server] grpc_port` in `crap.toml`).

## Message Size Limits

The maximum gRPC message size (both request and response) defaults to **16MB** — configurable via `grpc_max_message_size` in `[server]`. This is higher than Tonic's built-in 4MB default to accommodate large `Find` responses with deep relationship population.

```toml
[server]
grpc_max_message_size = "32MB"  # increase for very large responses
```

## Timeouts

An optional request timeout can be set via `grpc_timeout` in `[server]`. When set, RPCs exceeding the timeout return `DEADLINE_EXCEEDED`.

```toml
[server]
grpc_timeout = "30s"
```

## Bulk Operations

`UpdateMany` and `DeleteMany` are **atomic**: the whole operation runs in a single transaction, so a failure partway through (a validation error, a hook error, a constraint violation) rolls everything back — there is no partial-update state. They process the entire set the `where` clause matches; only the matching IDs are held in memory, not the full documents. Per-document lifecycle hooks run by default (set `hooks = false` to skip them).

Because the operation is atomic, a very large match-set holds the database write-lock for the whole operation. Set `[server] bulk_max_documents` to a positive value to cap how many documents a single bulk op may match (default `0` = no limit); a request matching more than the cap is rejected (gRPC `FAILED_PRECONDITION`) and changes nothing. See [crap.toml](../configuration/crap-toml.md).

## Server Reflection

When enabled (`grpc_reflection = true` in `[server]`, disabled by default), the server supports gRPC reflection, so tools like `grpcurl` work without importing the proto file:

```bash
# List services
grpcurl -plaintext localhost:50051 list

# Describe a service
grpcurl -plaintext localhost:50051 describe crap.ContentAPI

# Describe a message type
grpcurl -plaintext localhost:50051 describe crap.FindRequest
```

## Document Format

All documents use the same message format:

```protobuf
message Document {
  string id = 1;
  string collection = 2;
  google.protobuf.Struct fields = 3;
  optional string created_at = 4;
  optional string updated_at = 5;
}
```

The `fields` property is a `Struct` (JSON object) containing all user-defined field values.

## Testing with grpcurl

The repository includes `tests/api.sh` with grpcurl commands for every RPC:

```bash
source tests/api.sh
find_posts
create_post
find_post_by_id abc123
```

All commands use `-plaintext` (no TLS) and server reflection.
