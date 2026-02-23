# RPCs

All 14 RPCs with request/response shapes and grpcurl examples.

## Find

Find documents in a collection with filtering, sorting, and pagination.

```protobuf
message FindRequest {
  string collection = 1;
  map<string, string> filters = 2;      // simple key=value filters
  optional string order_by = 3;          // "-field" for descending
  optional int64 limit = 4;
  optional int64 offset = 5;
  optional string where = 6;            // JSON where clause (advanced)
  optional int32 depth = 7;             // population depth (default: 0)
}

message FindResponse {
  repeated Document documents = 1;
  int64 total = 2;
}
```

```bash
grpcurl -plaintext -d '{
    "collection": "posts",
    "filters": { "status": "published" },
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

## GetGlobal

Get a global's current value.

```protobuf
message GetGlobalRequest {
  string slug = 1;
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

Get the current authenticated user from a token.

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

Requires email configuration (`[email]` in `crap.toml`). If email is not configured, the token is still generated but no email is sent.

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
