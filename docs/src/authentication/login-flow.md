# Login Flow

## Admin UI Flow

1. User visits any `/admin/*` route
2. Auth middleware checks for `crap_session` HttpOnly cookie
3. If no valid cookie, redirects to `/admin/login`
4. User submits email + password
5. Server verifies credentials against the auth collection
6. On success: sets `crap_session` cookie with JWT, redirects to `/admin`
7. On failure: re-renders login page with error

**Public admin routes** (no auth required):
- `/admin/login`
- `/admin/logout`
- `/static/*`

**Custom strategy flow:**
If custom strategies are configured, the middleware checks them before redirecting to login:
1. JWT cookie check (fast path)
2. Custom strategies in definition order
3. Redirect to login (if all fail)

## gRPC Flow

### Login

```bash
grpcurl -plaintext -d '{
    "collection": "users",
    "email": "admin@example.com",
    "password": "secret123"
}' localhost:50051 crap.ContentAPI/Login
```

Response:

```json
{
    "token": "eyJhbGciOi...",
    "user": {
        "id": "abc123",
        "collection": "users",
        "fields": { "name": "Admin", "email": "admin@example.com", "role": "admin" }
    }
}
```

### Authenticated Requests

Pass the token via `authorization` metadata:

```bash
grpcurl -plaintext \
    -H "authorization: Bearer eyJhbGciOi..." \
    -d '{"collection": "posts"}' \
    localhost:50051 crap.ContentAPI/Find
```

### Get Current User

```bash
grpcurl -plaintext -d '{
    "token": "eyJhbGciOi..."
}' localhost:50051 crap.ContentAPI/Me
```

## Multiple Auth Collections

You can have multiple auth collections (e.g., `users` and `admins`). The `Login` RPC takes a `collection` parameter to specify which one to authenticate against.

The admin UI login always tries all auth collections.
