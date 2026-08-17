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
  optional bool trash = 13;             // true = return only soft-deleted documents
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
  optional bool trash = 7;   // true = allow finding soft-deleted documents
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
  DataMap data = 2;
  optional string locale = 3;           // locale code for localized fields
  optional bool draft = 4;              // true = create as draft (versioned collections)
  optional bool events = 5;             // default: true. false = quiet write (no live-update event)
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

## CreateMany

Bulk-create multiple documents in a single call. The whole batch is created in **one transaction** (atomic) — if any document fails, the entire batch rolls back and nothing is created. Per-document lifecycle hooks run by default — set `hooks=false` to skip them. The number of documents one call may create is capped by `[server] bulk_max_documents` (default `0` = unlimited); exceeding it errors and creates nothing. Returns the created count plus the created documents (with server-assigned IDs).

Unlike single [Create](#create), `CreateMany` **rejects** a `password` field for auth collections (`INVALID_ARGUMENT`) rather than silently dropping it. Set the password with a follow-up single `Create`/`Update`, which extract and policy-validate it.

```protobuf
message CreateManyRequest {
  string collection = 1;
  repeated DataMap documents = 2;  // documents to create
  optional string locale = 3;           // locale code for localized fields
  optional bool draft = 4;              // true = create as drafts
  optional bool hooks = 5;              // default: true. Set false to skip hooks.
  optional bool events = 6;             // default: false (bulk is quiet). true = emit per-doc events
}

message CreateManyResponse {
  int64 created = 1;                     // number of documents created
  repeated Document documents = 2;       // the created documents
}
```

```bash
grpcurl -plaintext -d '{
    "collection": "posts",
    "documents": [
        { "title": "First", "slug": "first" },
        { "title": "Second", "slug": "second" }
    ]
}' localhost:50051 crap.ContentAPI/CreateMany
```

## Update

Update an existing document.

```protobuf
message UpdateRequest {
  string collection = 1;
  string id = 2;
  DataMap data = 3;
  optional string locale = 4;           // locale code for localized fields
  optional bool draft = 5;              // true = version-only save (main table unchanged)
  optional bool unpublish = 6;          // true = set status to draft
  optional bool events = 7;             // default: true. false = quiet write (no live-update event)
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

Delete a document by ID. For collections with `soft_delete = true`, moves to trash by default. Set `force_hard_delete = true` to permanently delete.

```protobuf
message DeleteRequest {
  string collection = 1;
  string id = 2;
  bool force_hard_delete = 3;  // permanently delete even with soft_delete
  optional bool events = 4;    // default: true. false = quiet delete (no live-update event)
}

message DeleteResponse {
  bool soft_deleted = 2;       // true if moved to trash (not permanently deleted)
}
```

```bash
# Soft delete (moves to trash)
grpcurl -plaintext -d '{
    "collection": "posts",
    "id": "abc123"
}' localhost:50051 crap.ContentAPI/Delete

# Force permanent delete
grpcurl -plaintext -d '{
    "collection": "posts",
    "id": "abc123",
    "force_hard_delete": true
}' localhost:50051 crap.ContentAPI/Delete
```

## Undelete

Undelete a soft-deleted document from trash. Only works on collections with `soft_delete = true`.

```protobuf
message UndeleteRequest {
  string collection = 1;
  string id = 2;
}

message UndeleteResponse {
  Document document = 1;
}
```

```bash
grpcurl -plaintext -d '{
    "collection": "posts",
    "id": "abc123"
}' localhost:50051 crap.ContentAPI/Undelete
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
  optional bool trash = 6;              // true = count soft-deleted (trashed) docs instead of live; default false
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
  DataMap data = 3;      // field values to apply
  optional string locale = 4;           // locale code for localized fields
  optional bool draft = 5;              // true = save as drafts
  optional bool hooks = 6;              // default: true. Set false to skip hooks & validation.
  optional bool events = 7;             // default: false (bulk is quiet). true = emit per-doc events
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

By default there is no per-call document limit; set `[server] bulk_max_documents` to cap how many documents a single bulk op may match (`0` = unlimited). Only the matching IDs are held in memory (not the full documents). A request matching more than the cap is rejected with `FAILED_PRECONDITION` and changes nothing.

## DeleteMany

Bulk-delete all documents matching a filter. All deletions run in a single transaction (all-or-nothing). Fires per-document hooks by default. Respects the collection's `soft_delete` setting — documents are moved to trash unless `force_hard_delete` is set.

```protobuf
message DeleteManyRequest {
  string collection = 1;
  optional string where = 2;            // JSON where clause (omit = all docs)
  optional bool hooks = 3;              // default: true. Set false to skip hooks.
  bool force_hard_delete = 4;           // permanently delete even if soft_delete is enabled
  optional bool events = 5;             // default: false (bulk is quiet). true = emit per-doc events
}

message DeleteManyResponse {
  int64 deleted = 1;                    // permanently deleted count
  int64 soft_deleted = 2;              // soft-deleted (trashed) count
  int64 skipped = 3;                   // skipped because still referenced by other documents
}
```

```bash
grpcurl -plaintext -d '{
    "collection": "posts",
    "where": "{\"status\": \"archived\"}"
}' localhost:50051 crap.ContentAPI/DeleteMany
```

The operation is atomic — a real failure rolls everything back — while documents still referenced by others are **skipped** (counted in `skipped`), not failed. By default there is no per-call document limit; set `[server] bulk_max_documents` to cap how many documents a single bulk op may match (`0` = unlimited). A request matching more than the cap is rejected with `FAILED_PRECONDITION` and changes nothing.

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
  DataMap data = 2;
  optional string locale = 3;           // locale code for localized fields
  optional bool events = 4;             // default: true. false = quiet write (no live-update event)
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

Initiate a password reset flow. Generates a reset token and sends a reset email. Always returns an empty (non-error) response regardless of whether the email exists, to prevent user enumeration.

```protobuf
message ForgotPasswordRequest {
  string collection = 1;
  string email = 2;
}

message ForgotPasswordResponse {}
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

message ResetPasswordResponse {}
```

```bash
grpcurl -plaintext -d '{
    "collection": "users",
    "token": "the-reset-token",
    "new_password": "newsecret123"
}' localhost:50051 crap.ContentAPI/ResetPassword
```

Tokens are single-use and expire after `reset_token_expiry` seconds (default: 3600 = 1 hour, configurable in `[auth]`).

## VerifyEmail

Verify a user's email address using a token sent during account creation.

```protobuf
message VerifyEmailRequest {
  string collection = 1;
  string token = 2;
}

message VerifyEmailResponse {}
```

```bash
grpcurl -plaintext -d '{
    "collection": "users",
    "token": "the-verification-token"
}' localhost:50051 crap.ContentAPI/VerifyEmail
```

Only relevant for auth collections with `verify_email: true`.

## Validate

Check field data against collection rules without persisting. Runs field validation (required, unique, type checks, custom validators) but does not open a write transaction. Use this to preview validation errors before a `Create` or `Update` call.

```protobuf
message ValidateRequest {
  string collection = 1;
  DataMap data = 2;
  optional bool draft = 3;              // relaxes required-field checks for draft collections
  optional string locale = 4;           // locale code for localized field validation
  optional string id = 5;               // when set, exclude this ID from unique checks (update path)
}

message ValidateResponse {
  bool valid = 1;
  map<string, string> errors = 2;       // per-field error messages; empty when valid
}
```

```bash
grpcurl -plaintext -d '{
    "collection": "posts",
    "data": { "title": "Hello", "slug": "hello" }
}' localhost:50051 crap.ContentAPI/Validate
```

**Access:** optional. If a Bearer token is present, the collection's `create`/`update` access function is evaluated (`update` when `id` is set). Field-level write-denied fields are stripped before validation runs.

## ValidateGlobal

Check global field data against its rules without persisting. The global equivalent of `Validate`. Globals are a singleton document, so validation always runs in update mode against the fixed `default` row — there is no create mode and no `id` field. Returns the same `ValidateResponse` as `Validate`.

```protobuf
message ValidateGlobalRequest {
  string slug = 1;
  DataMap data = 2;
  optional bool draft = 3;              // relaxes required-field checks for globals with drafts enabled
  optional string locale = 4;           // locale code for localized field validation
}
```

```bash
grpcurl -plaintext -d '{
    "slug": "settings",
    "data": { "site_name": "My CMS" }
}' localhost:50051 crap.ContentAPI/ValidateGlobal
```

**Access:** optional. If a Bearer token is present, the global's `update` access function is evaluated. Field-level write-denied fields are stripped before validation runs.

## LockAccount

Prevent a user from logging in. Only valid for auth-enabled collections. Any active live-update streams owned by the locked user are torn down with `PermissionDenied`.

```protobuf
message AccountActionRequest {
  string collection = 1;                // auth collection slug
  string id = 2;                        // target user's nanoid ID
}

message AccountActionResponse {}
```

```bash
grpcurl -plaintext -H "authorization: Bearer $TOKEN" -d '{
    "collection": "users",
    "id": "usr_abc123"
}' localhost:50051 crap.ContentAPI/LockAccount
```

**Access:** authentication required. Returns `INVALID_ARGUMENT` if the collection is not an auth collection, `NOT_FOUND` if the user ID is missing, and `UNAUTHENTICATED` if no valid Bearer token is provided.

`AccountActionResponse` is empty: success is signalled by a non-error response. Failures are reported as gRPC status errors.

## UnlockAccount

Re-enable login for a previously locked account. Only valid for auth-enabled collections.

```protobuf
message AccountActionRequest {
  string collection = 1;
  string id = 2;
}

message AccountActionResponse {}
```

```bash
grpcurl -plaintext -H "authorization: Bearer $TOKEN" -d '{
    "collection": "users",
    "id": "usr_abc123"
}' localhost:50051 crap.ContentAPI/UnlockAccount
```

**Access:** authentication required. Same error semantics as `LockAccount`.

## VerifyAccount

Mark a user's email as verified without requiring the user to click the verification link. Only valid for auth-enabled collections.

```protobuf
message AccountActionRequest {
  string collection = 1;
  string id = 2;
}

message AccountActionResponse {}
```

```bash
grpcurl -plaintext -H "authorization: Bearer $TOKEN" -d '{
    "collection": "users",
    "id": "usr_abc123"
}' localhost:50051 crap.ContentAPI/VerifyAccount
```

**Access:** authentication required. Same error semantics as `LockAccount`. See `VerifyEmail` for the token-based flow triggered by the user.

## UnverifyAccount

Mark a user's email as unverified. Only valid for auth-enabled collections.

```protobuf
message AccountActionRequest {
  string collection = 1;
  string id = 2;
}

message AccountActionResponse {}
```

```bash
grpcurl -plaintext -H "authorization: Bearer $TOKEN" -d '{
    "collection": "users",
    "id": "usr_abc123"
}' localhost:50051 crap.ContentAPI/UnverifyAccount
```

**Access:** authentication required. Same error semantics as `LockAccount`.

## ListCollections

List all collections and globals (lightweight overview).

**Access:** public by default — any caller may read the content model, as in a headless CMS. Set `[server] public_schema_introspection = false` to require an authenticated caller (an anonymous request then gets `UNAUTHENTICATED`). This gates only the schema shape; document data is always access-gated.

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

**Access:** same gate as [ListCollections](#listcollections) — public by default; require auth with `[server] public_schema_introspection = false`.

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
  PaginationInfo pagination = 2;
}

enum VersionStatus {
  VERSION_STATUS_UNSPECIFIED = 0;
  VERSION_STATUS_PUBLISHED = 1;
  VERSION_STATUS_DRAFT = 2;
}

message VersionInfo {
  string id = 1;
  int64 version = 2;
  VersionStatus status = 3;
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

enum MutationTarget {
  MUTATION_TARGET_UNSPECIFIED = 0;
  MUTATION_TARGET_COLLECTION = 1;
  MUTATION_TARGET_GLOBAL = 2;
}

enum MutationOperation {
  MUTATION_OPERATION_UNSPECIFIED = 0;
  MUTATION_OPERATION_CREATE = 1;
  MUTATION_OPERATION_UPDATE = 2;
  MUTATION_OPERATION_DELETE = 3;
}

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
  optional int32 priority = 3;    // scheduling priority (higher = sooner; default 0)
}

message TriggerJobResponse {
  string job_id = 1;  // the queued job run ID
}
```

```bash
grpcurl -plaintext -H "authorization: Bearer $TOKEN" -d '{
    "slug": "cleanup_expired",
    "data_json": "{\"force\": true}",
    "priority": 10
}' localhost:50051 crap.ContentAPI/TriggerJob
```

## GetJobRun

Get details of a specific job run. Requires authentication.

```protobuf
message GetJobRunRequest {
  string id = 1;
}

enum JobRunStatus {
  JOB_RUN_STATUS_UNSPECIFIED = 0;
  JOB_RUN_STATUS_PENDING = 1;
  JOB_RUN_STATUS_RUNNING = 2;
  JOB_RUN_STATUS_COMPLETED = 3;
  JOB_RUN_STATUS_FAILED = 4;
  JOB_RUN_STATUS_STALE = 5;
}

message JobRunInfo {
  string id = 1;
  string slug = 2;
  JobRunStatus status = 3;            // was a string; now a JobRunStatus enum
  string data_json = 4;
  optional string result_json = 5;
  optional string error = 6;
  uint32 attempt = 7;
  uint32 max_attempts = 8;
  JobScheduledBy scheduled_by = 9;    // enum, NOT optional
  optional string created_at = 10;
  optional string started_at = 11;
  optional string completed_at = 12;
  int32 priority = 13;
  optional string unique_key = 14;
}

message GetJobRunResponse {
  JobRunInfo run = 1;
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
  JobRunStatus status = 2;             // UNSPECIFIED = all statuses
  optional int64 limit = 3;
  optional int64 offset = 4;
}

message ListJobRunsResponse {
  repeated JobRunInfo runs = 1;
  PaginationInfo pagination = 2;
}
```

```bash
# List all recent job runs
grpcurl -plaintext -H "authorization: Bearer $TOKEN" -d '{}' \
    localhost:50051 crap.ContentAPI/ListJobRuns

# Filter by slug and status
grpcurl -plaintext -H "authorization: Bearer $TOKEN" -d '{
    "slug": "cleanup_expired",
    "status": "JOB_RUN_STATUS_COMPLETED",
    "limit": "20"
}' localhost:50051 crap.ContentAPI/ListJobRuns
```
