# gRPC API

Crap CMS exposes a gRPC API via Tonic for programmatic access to all content operations.

## Service

```protobuf
service ContentAPI {
  rpc Find (FindRequest) returns (FindResponse);
  rpc FindByID (FindByIDRequest) returns (FindByIDResponse);
  rpc Create (CreateRequest) returns (CreateResponse);
  rpc Update (UpdateRequest) returns (UpdateResponse);
  rpc Delete (DeleteRequest) returns (DeleteResponse);
  rpc Count (CountRequest) returns (CountResponse);
  rpc UpdateMany (UpdateManyRequest) returns (UpdateManyResponse);
  rpc DeleteMany (DeleteManyRequest) returns (DeleteManyResponse);
  rpc GetGlobal (GetGlobalRequest) returns (GetGlobalResponse);
  rpc UpdateGlobal (UpdateGlobalRequest) returns (UpdateGlobalResponse);
  rpc Login (LoginRequest) returns (LoginResponse);
  rpc Me (MeRequest) returns (MeResponse);
  rpc ForgotPassword (ForgotPasswordRequest) returns (ForgotPasswordResponse);
  rpc ResetPassword (ResetPasswordRequest) returns (ResetPasswordResponse);
  rpc VerifyEmail (VerifyEmailRequest) returns (VerifyEmailResponse);
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

## Server Reflection

The server supports gRPC reflection, so tools like `grpcurl` work without importing the proto file:

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
