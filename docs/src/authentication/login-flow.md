# Login Flow

## Admin UI Flow

1. User visits any `/admin/*` route
2. Auth middleware checks for `crap_session` HttpOnly cookie (includes `Secure` flag when `dev_mode = false`)
3. If no valid cookie, redirects to `/admin/login`
4. User submits email + password (protected by CSRF double-submit cookie)
5. Server checks rate limiting — too many failed attempts for this email triggers a temporary lockout
6. Server verifies credentials against the auth collection (constant-time, even for non-existent users)
7. On success: clears rate limit counter, sets `crap_session` cookie with JWT, redirects to `/admin`
8. On failure: records failed attempt, re-renders login page with error

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

## Security

### Rate Limiting

Login endpoints enforce per-email rate limiting. After `max_login_attempts` (default: 5) failed attempts within the lockout window, further login attempts for that email are temporarily blocked for `login_lockout_seconds` (default: 300). Configure in `crap.toml`:

```toml
[auth]
max_login_attempts = 5
login_lockout_seconds = 300
```

Rate limiting applies to both the admin UI login and the gRPC `Login` RPC.

### CSRF Protection

All admin UI form submissions and HTMX requests are protected by a double-submit cookie pattern:

- A `crap_csrf` cookie (SameSite=Strict, not HttpOnly) is set on every response
- POST, PUT, and DELETE requests must include a matching token via either:
  - `X-CSRF-Token` header (used by HTMX requests)
  - `_csrf` form field (used by plain form submissions)
- Mismatched or missing tokens return 403 Forbidden

This is handled automatically by JavaScript included in the admin templates.

### Timing Safety

Login always performs a full Argon2id hash comparison, even when the requested email doesn't exist. This prevents timing-based user enumeration attacks.

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
3. Verification tokens expire after **24 hours**
4. User clicks the verification link (expired tokens show an error)
5. Login attempts before verification return "Please verify your email"

### gRPC

```bash
grpcurl -plaintext -d '{
    "collection": "users",
    "token": "the-token-from-email"
}' localhost:50051 crap.ContentAPI/VerifyEmail
```
