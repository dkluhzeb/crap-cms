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
- `/admin/forgot-password`
- `/admin/reset-password`
- `/admin/verify-email`
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

## Password Reset Flow

When email is configured (`[email]` section in `crap.toml`):

### Admin UI

1. User clicks "Forgot password?" on the login page
2. Enters their email address and selects the auth collection
3. Server generates a nanoid reset token with 1-hour expiry
4. Reset email is sent with a link to `/admin/reset-password?token=xxx`
5. User clicks the link, enters a new password
6. Server validates the token, updates the password, and redirects to login

### gRPC

```bash
# Step 1: Request password reset
grpcurl -plaintext -d '{
    "collection": "users",
    "email": "admin@example.com"
}' localhost:50051 crap.ContentAPI/ForgotPassword

# Step 2: Reset password with token from email
grpcurl -plaintext -d '{
    "collection": "users",
    "token": "the-token-from-email",
    "new_password": "newsecret123"
}' localhost:50051 crap.ContentAPI/ResetPassword
```

**Note:** `ForgotPassword` always returns success to prevent user enumeration.

## Email Verification Flow

When `verify_email: true` is set on an auth collection:

### Admin UI

1. User is created (via admin form or gRPC)
2. Verification email is sent automatically with a link to `/admin/verify-email?token=xxx`
3. User clicks the verification link
4. Login attempts before verification return "Please verify your email"

### gRPC

```bash
grpcurl -plaintext -d '{
    "collection": "users",
    "token": "the-token-from-email"
}' localhost:50051 crap.ContentAPI/VerifyEmail
```
