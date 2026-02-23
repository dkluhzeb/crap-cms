# gRPC Authentication

## Login

Authenticate with email and password to get a JWT token:

```bash
grpcurl -plaintext -d '{
    "collection": "users",
    "email": "admin@example.com",
    "password": "secret123"
}' localhost:50051 crap.ContentAPI/Login
```

The response contains a `token` and the `user` document.

## Bearer Token

Pass the token via the `authorization` metadata header:

```bash
grpcurl -plaintext \
    -H "authorization: Bearer eyJhbGciOi..." \
    -d '{"collection": "posts"}' \
    localhost:50051 crap.ContentAPI/Find
```

The token is extracted from the `authorization` metadata and validated. The authenticated user is available to access control functions.

## Get Current User

Use the `Me` RPC to validate a token and get the user:

```bash
grpcurl -plaintext -d '{
    "token": "eyJhbGciOi..."
}' localhost:50051 crap.ContentAPI/Me
```

## Token Expiry

Tokens expire after `token_expiry` seconds (default: 7200 = 2 hours). Configurable globally in `crap.toml` or per auth collection.

## Creating Users via gRPC

Include `password` in the `data` field of a `Create` request:

```bash
grpcurl -plaintext -d '{
    "collection": "users",
    "data": {
        "email": "new@example.com",
        "password": "secret123",
        "name": "New User",
        "role": "editor"
    }
}' localhost:50051 crap.ContentAPI/Create
```

The `password` field is extracted, hashed with Argon2id, and stored separately. It never appears in the response.

## Updating Passwords

Include `password` in the `data` field of an `Update` request:

```bash
grpcurl -plaintext -d '{
    "collection": "users",
    "id": "abc123",
    "data": { "password": "new-password" }
}' localhost:50051 crap.ContentAPI/Update
```

If `password` is omitted, the existing password is kept.
