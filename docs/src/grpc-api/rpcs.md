# RPCs

All RPCs with request/response shapes and grpcurl examples.

## Find

Find documents in a collection with filtering, sorting, and pagination.

```protobuf
message FindRequest {
  string collection = 1;
  optional string where = 2;            // JSON where clause
  optional string order_by = 3;         // "-field" for descending
  optional int64 limit = 4;
  optional int64 page = 5;             // page number (1-based, default: 1)
  optional int32 depth = 6;             // population depth (default: 0)
  optional string locale = 7;           // locale code for localized fields
  repeated string select = 8;           // fields to return (empty = all)
  optional bool draft = 9;              // true = include drafts (versioned collections)
  optional string after_cursor = 10;    // opaque forward cursor for cursor-based pagination
  optional string before_cursor = 11;   // opaque backward cursor for cursor-based pagination
  optional string search = 12;          // FTS5 full-text search query
}

message PaginationInfo {
  int64 total_docs = 1;                 // total matching documents (before limit/page)
  int64 limit = 2;                      // applied limit
  optional int64 total_pages = 3;      // total pages (page mode only)
  optional int64 page = 4;             // current page (page mode only, 1-based)
  optional int64 page_start = 5;      // 1-based index of first doc on this page (page mode only)
  bool has_prev_page = 6;              // whether a previous page exists
  bool has_next_page = 7;              // whether a next page exists
  optional int64 prev_page = 8;        // previous page number (nil if first page)
  optional int64 next_page = 9;        // next page number (nil if last page)
  optional string start_cursor = 10;   // opaque cursor of first doc in results (cursor mode only)
  optional string end_cursor = 11;     // opaque cursor of last doc in results (cursor mode only)
}

message FindResponse {
  repeated Document documents = 1;
  PaginationInfo pagination = 2;
}
```

Pagination metadata is nested in a `PaginationInfo` message. In page mode (default), `page`, `total_pages`, `page_start`, `prev_page`, and `next_page` are computed. In cursor mode, `start_cursor` and `end_cursor` are provided instead — these are the cursors of the first and last documents in the result set. `has_prev_page` and `has_next_page` work in both modes.

`after_cursor`/`before_cursor` and `page` are mutually exclusive. `after_cursor` and `before_cursor` are also mutually exclusive with each other. Cursors are only present in the response when `[pagination] mode = "cursor"` is set in `crap.toml`.

```bash
grpcurl -plaintext -d '{
    "collection": "posts",
    "where": "{\"status\": \"published\"}",
    "order_by": "-created_at",
    "limit": "10",
    "depth": 1
}' localhost:50051 crap.ContentAPI/Find
```

## FindByID

Get a single document by ID.

```protobuf
message FindByIDRequest {
  string collection = 1;
  string id = 2;
  optional int32 depth = 3;  // default: depth.default_depth from crap.toml
  optional string locale = 4;  // locale code for localized fields
  repeated string select = 5;  // fields to return (empty = all)
  optional bool draft = 6;   // true = return latest version (may be draft)
}

message FindByIDResponse {
  optional Document document = 1;
}
```

```bash
grpcurl -plaintext -d '{
    "collection": "posts",
    "id": "abc123",
    "depth": 2
}' localhost:50051 crap.ContentAPI/FindByID
```

## Create

Create a new document.

```protobuf
message CreateRequest {
  string collection = 1;
  google.protobuf.Struct data = 2;
  optional string locale = 3;           // locale code for localized fields
  optional bool draft = 4;              // true = create as draft (versioned collections)
}

message CreateResponse {
  Document document = 1;
}
```

```bash
grpcurl -plaintext -d '{
    "collection": "posts",
    "data": {
        "title": "Hello World",
        "slug": "hello-world",
        "status": "draft"
    }
}' localhost:50051 crap.ContentAPI/Create
```

For auth collections, include `password` in the data to set the user's password.

## Update

Update an existing document.

```protobuf
message UpdateRequest {
  string collection = 1;
  string id = 2;
  google.protobuf.Struct data = 3;
  optional string locale = 4;           // locale code for localized fields
  optional bool draft = 5;              // true = version-only save (main table unchanged)
  optional bool unpublish = 6;          // true = set status to draft
}

message UpdateResponse {
  Document document = 1;
}
```

```bash
grpcurl -plaintext -d '{
    "collection": "posts",
    "id": "abc123",
    "data": { "title": "Updated Title", "status": "published" }
}' localhost:50051 crap.ContentAPI/Update
```

## Delete

Delete a document by ID.

```protobuf
message DeleteRequest {
  string collection = 1;
  string id = 2;
}

message DeleteResponse {
  bool success = 1;
}
```

```bash
grpcurl -plaintext -d '{
    "collection": "posts",
    "id": "abc123"
}' localhost:50051 crap.ContentAPI/Delete
```

## Count

Count documents matching an optional filter. Respects collection-level read access.

```protobuf
message CountRequest {
  string collection = 1;
  optional string where = 2;            // JSON where clause
  optional string locale = 3;           // locale code for localized field filtering
  optional bool draft = 4;              // true = include drafts
  optional string search = 5;           // FTS5 full-text search query
}

message CountResponse {
  int64 count = 1;
}
```

```bash
grpcurl -plaintext -d '{
    "collection": "posts",
    "where": "{\"status\": \"published\"}"
}' localhost:50051 crap.ContentAPI/Count
```

## UpdateMany

Bulk-update all documents matching a filter. All updates run in a single transaction (all-or-nothing). Runs the full per-document lifecycle by default: `before_validate` → field validation → `before_change` → DB update → `after_change` — the same pipeline as single-document `Update`.

Only provided fields are written (partial update). Absent fields are left unchanged — including checkbox fields, which are **not** reset to `0` as they would be in a full single-document update.

Password updates are rejected in bulk operations. Use single-document `Update` instead.

```protobuf
message UpdateManyRequest {
  string collection = 1;
  optional string where = 2;            // JSON where clause (omit = all docs)
  google.protobuf.Struct data = 3;      // field values to apply
  optional string locale = 4;           // locale code for localized fields
  optional bool draft = 5;              // true = save as drafts
  optional bool hooks = 6;              // default: true. Set false to skip hooks & validation.
}

message UpdateManyResponse {
  int64 modified = 1;
}
```

```bash
grpcurl -plaintext -d '{
    "collection": "posts",
    "where": "{\"status\": \"draft\"}",
    "data": { "status": "published" }
}' localhost:50051 crap.ContentAPI/UpdateMany
```

## DeleteMany

Bulk-delete all documents matching a filter. All deletions run in a single transaction (all-or-nothing). Fires per-document hooks by default.

```protobuf
message DeleteManyRequest {
  string collection = 1;
  optional string where = 2;            // JSON where clause (omit = all docs)
  optional bool hooks = 3;              // default: true. Set false to skip hooks.
}

message DeleteManyResponse {
  int64 deleted = 1;
}
```

```bash
grpcurl -plaintext -d '{
    "collection": "posts",
    "where": "{\"status\": \"archived\"}"
}' localhost:50051 crap.ContentAPI/DeleteMany
```

## GetGlobal

Get a global's current value.

```protobuf
message GetGlobalRequest {
  string slug = 1;
  optional string locale = 2;           // locale code for localized fields
}

message GetGlobalResponse {
  Document document = 1;
}
```

```bash
grpcurl -plaintext -d '{"slug": "site_settings"}' \
    localhost:50051 crap.ContentAPI/GetGlobal
```

## UpdateGlobal

Update a global's value.

```protobuf
message UpdateGlobalRequest {
  string slug = 1;
  google.protobuf.Struct data = 2;
  optional string locale = 3;           // locale code for localized fields
}

message UpdateGlobalResponse {
  Document document = 1;
}
```

```bash
grpcurl -plaintext -d '{
    "slug": "site_settings",
    "data": { "site_name": "Updated Name" }
}' localhost:50051 crap.ContentAPI/UpdateGlobal
```

## Login

Authenticate with email and password. Returns a JWT token and user document.

```protobuf
message LoginRequest {
  string collection = 1;
  string email = 2;
  string password = 3;
}

message LoginResponse {
  string token = 1;
  Document user = 2;
}
```

```bash
grpcurl -plaintext -d '{
    "collection": "users",
    "email": "admin@example.com",
    "password": "secret123"
}' localhost:50051 crap.ContentAPI/Login
```

## Me

Get the current authenticated user from a token. The token is read from the `authorization` metadata header first; if absent, falls back to the `token` field in the request body.

```protobuf
message MeRequest {
  string token = 1;
}

message MeResponse {
  Document user = 1;
}
```

```bash
grpcurl -plaintext -d '{
    "token": "eyJhbGciOi..."
}' localhost:50051 crap.ContentAPI/Me
```

## ForgotPassword

Initiate a password reset flow. Generates a reset token and sends a reset email. Always returns success to prevent user enumeration.

```protobuf
message ForgotPasswordRequest {
  string collection = 1;
  string email = 2;
}

message ForgotPasswordResponse {
  bool success = 1;  // always true
}
```

```bash
grpcurl -plaintext -d '{
    "collection": "users",
    "email": "admin@example.com"
}' localhost:50051 crap.ContentAPI/ForgotPassword
```

Requires email configuration (`[email]` in `crap.toml`). Without email configured, the reset token is generated and stored but never delivered — the forgot-password flow is non-functional without SMTP.

## ResetPassword

Reset a user's password using a token from the reset email.

```protobuf
message ResetPasswordRequest {
  string collection = 1;
  string token = 2;
  string new_password = 3;
}

message ResetPasswordResponse {
  bool success = 1;
}
```

```bash
grpcurl -plaintext -d '{
    "collection": "users",
    "token": "the-reset-token",
    "new_password": "newsecret123"
}' localhost:50051 crap.ContentAPI/ResetPassword
```

Tokens are single-use and expire after 1 hour.

## VerifyEmail

Verify a user's email address using a token sent during account creation.

```protobuf
message VerifyEmailRequest {
  string collection = 1;
  string token = 2;
}

message VerifyEmailResponse {
  bool success = 1;
}
```

```bash
grpcurl -plaintext -d '{
    "collection": "users",
    "token": "the-verification-token"
}' localhost:50051 crap.ContentAPI/VerifyEmail
```

Only relevant for auth collections with `verify_email: true`.

## ListCollections

List all collections and globals (lightweight overview).

```protobuf
message ListCollectionsRequest {}

message ListCollectionsResponse {
  repeated CollectionInfo collections = 1;
  repeated GlobalInfo globals = 2;
}
```

```bash
grpcurl -plaintext -d '{}' localhost:50051 crap.ContentAPI/ListCollections
```

## DescribeCollection

Get full field schema for a collection or global.

```protobuf
message DescribeCollectionRequest {
  string slug = 1;
  bool is_global = 2;
}

message DescribeCollectionResponse {
  string slug = 1;
  optional string singular_label = 2;
  optional string plural_label = 3;
  bool timestamps = 4;
  bool auth = 5;
  repeated FieldInfo fields = 6;
  bool upload = 7;
  bool drafts = 8;  // true if collection has versions with drafts enabled
}
```

```bash
# Describe a collection
grpcurl -plaintext -d '{"slug": "posts"}' \
    localhost:50051 crap.ContentAPI/DescribeCollection

# Describe a global
grpcurl -plaintext -d '{"slug": "site_settings", "is_global": true}' \
    localhost:50051 crap.ContentAPI/DescribeCollection
```

## ListVersions

List version history for a document. Only available for versioned collections.

```protobuf
message ListVersionsRequest {
  string collection = 1;
  string id = 2;
  optional int64 limit = 3;
}

message ListVersionsResponse {
  repeated VersionInfo versions = 1;
}

message VersionInfo {
  string id = 1;
  int64 version = 2;
  string status = 3;      // "published" or "draft"
  bool latest = 4;
  string created_at = 5;
}
```

```bash
grpcurl -plaintext -d '{
    "collection": "articles",
    "id": "abc123",
    "limit": "10"
}' localhost:50051 crap.ContentAPI/ListVersions
```

Returns versions in newest-first order. Returns an error for non-versioned collections.

## RestoreVersion

Restore a previous version, writing its snapshot data back to the main table.

```protobuf
message RestoreVersionRequest {
  string collection = 1;
  string document_id = 2;
  string version_id = 3;
}

message RestoreVersionResponse {
  Document document = 1;
}
```

```bash
grpcurl -plaintext -d '{
    "collection": "articles",
    "document_id": "abc123",
    "version_id": "v_xyz"
}' localhost:50051 crap.ContentAPI/RestoreVersion
```

This overwrites the main table with the version's snapshot, sets `_status` to `"published"`, and creates a new version entry for the restore. Returns an error for non-versioned collections.

## Subscribe

Subscribe to real-time mutation events (server streaming). See [Live Updates](../live-updates/grpc-streaming.md) for full documentation.

```protobuf
message SubscribeRequest {
  repeated string collections = 1;  // empty = all accessible
  repeated string globals = 2;      // empty = all accessible
  repeated string operations = 3;   // "create","update","delete" — empty = all
  string token = 4;                 // auth token
}

message MutationEvent {
  uint64 sequence = 1;
  string timestamp = 2;
  string target = 3;
  string operation = 4;
  string collection = 5;
  string document_id = 6;
  google.protobuf.Struct data = 7;
}
```

```bash
# Subscribe to all events
grpcurl -plaintext -d '{}' \
    localhost:50051 crap.ContentAPI/Subscribe

# Subscribe to specific collections with auth
grpcurl -plaintext -d '{
    "collections": ["posts"],
    "operations": ["create", "update"],
    "token": "your-jwt-token"
}' localhost:50051 crap.ContentAPI/Subscribe
```

## ListJobs

List all defined jobs and their configuration. Requires authentication.

```protobuf
message ListJobsRequest {}

message ListJobsResponse {
  repeated JobDefinitionInfo jobs = 1;
}

message JobDefinitionInfo {
  string slug = 1;
  string handler = 2;
  optional string schedule = 3;
  string queue = 4;
  uint32 retries = 5;
  uint64 timeout = 6;
  uint32 concurrency = 7;
  bool skip_if_running = 8;
  optional string label = 9;
}
```

```bash
grpcurl -plaintext -H "authorization: Bearer $TOKEN" -d '{}' \
    localhost:50051 crap.ContentAPI/ListJobs
```

## TriggerJob

Queue a job for execution. Requires authentication. Checks the job's `access` function if defined.

```protobuf
message TriggerJobRequest {
  string slug = 1;
  optional string data_json = 2;  // JSON input data
}

message TriggerJobResponse {
  string job_id = 1;  // the queued job run ID
}
```

```bash
grpcurl -plaintext -H "authorization: Bearer $TOKEN" -d '{
    "slug": "cleanup_expired",
    "data_json": "{\"force\": true}"
}' localhost:50051 crap.ContentAPI/TriggerJob
```

## GetJobRun

Get details of a specific job run. Requires authentication.

```protobuf
message GetJobRunRequest {
  string id = 1;
}

message GetJobRunResponse {
  string id = 1;
  string slug = 2;
  string status = 3;
  string data_json = 4;
  optional string result_json = 5;
  optional string error = 6;
  uint32 attempt = 7;
  uint32 max_attempts = 8;
  optional string scheduled_by = 9;
  optional string created_at = 10;
  optional string started_at = 11;
  optional string completed_at = 12;
}
```

```bash
grpcurl -plaintext -H "authorization: Bearer $TOKEN" -d '{
    "id": "job_run_id_here"
}' localhost:50051 crap.ContentAPI/GetJobRun
```

## ListJobRuns

List job runs with optional filters. Requires authentication.

```protobuf
message ListJobRunsRequest {
  optional string slug = 1;
  optional string status = 2;
  optional int64 limit = 3;
  optional int64 offset = 4;
}

message ListJobRunsResponse {
  repeated GetJobRunResponse runs = 1;
}
```

```bash
# List all recent job runs
grpcurl -plaintext -H "authorization: Bearer $TOKEN" -d '{}' \
    localhost:50051 crap.ContentAPI/ListJobRuns

# Filter by slug and status
grpcurl -plaintext -H "authorization: Bearer $TOKEN" -d '{
    "slug": "cleanup_expired",
    "status": "completed",
    "limit": "20"
}' localhost:50051 crap.ContentAPI/ListJobRuns
```
