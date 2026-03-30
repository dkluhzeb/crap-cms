# Login Flow

## Admin UI Flow

1. User visits any `/admin/*` route
2. **Gate 1: `require_auth` check** — if no auth collections exist and `require_auth` is `true` (default), returns a "Setup Required" page (HTTP 503). Set `require_auth = false` in `[admin]` for open dev mode.
3. Auth middleware checks for `crap_session` HttpOnly cookie (includes `Secure` flag when `dev_mode = false`)
4. If no valid cookie, tries custom auth strategies, then redirects to `/admin/login`
5. **Gate 2: `admin.access` check** — if an `access` Lua function is configured in `[admin]`, it runs after successful authentication. If the function returns `false`/`nil`, the user sees an "Access Denied" page (HTTP 403) with a logout button.
6. User submits email + password (protected by CSRF double-submit cookie)
7. Server checks rate limiting — too many failed attempts for this email triggers a temporary lockout
8. Server verifies credentials against the auth collection (constant-time, even for non-existent users)
9. On success: clears rate limit counter, sets `crap_session` cookie with JWT, redirects to `/admin`
10. On failure: records failed attempt, re-renders login page with error

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

Login and forgot-password endpoints enforce dual rate limiting — per-email and per-IP:

- **Per-email**: After `max_login_attempts` (default: 5) failed attempts, further login attempts for that email are blocked for `login_lockout_seconds` (default: 300s).
- **Per-IP**: After `max_ip_login_attempts` (default: 20) failed attempts from the same IP, all login attempts from that IP are blocked. The higher threshold tolerates shared IPs (offices, NAT).

Forgot-password requests are similarly limited per-email (`max_forgot_password_attempts`) and per-IP (`max_ip_login_attempts` with `forgot_password_window_seconds`).

```toml
[auth]
max_login_attempts = 5          # per-email threshold
max_ip_login_attempts = 20      # per-IP threshold (login + forgot-password)
login_lockout_seconds = "5m"    # lockout window for login
max_forgot_password_attempts = 3
forgot_password_window_seconds = "15m"
```

Rate limiting applies to the admin UI login, admin forgot-password, and the gRPC `Login` and `ForgotPassword` RPCs. Behind a reverse proxy, the admin UI reads the client IP from `X-Forwarded-For`.

### CSRF Protection

All admin UI form submissions and HTMX requests are protected by a double-submit cookie pattern:

- A `crap_csrf` cookie (SameSite=Strict, not HttpOnly) is set when absent (persists with a 24-hour Max-Age)
- POST, PUT, PATCH, and DELETE requests must include a matching token via either:
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
