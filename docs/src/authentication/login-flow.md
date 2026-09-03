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
- `/admin/mfa` (requires a pending MFA challenge cookie, not a session)
- `/admin/auth/callback/{name}` and `/admin/auth/callback/{collection}/{name}` (OAuth/OIDC callbacks)
- `/static/*`, `/health`, `/ready`

**Request authentication:** every other `/admin/**` request runs the shared auth evaluator with the fixed precedence described in [Auth Methods](auth-methods.md#evaluation-order): session cookie → always-active strategies → header-activated strategies. A cookie that decodes but is invalid (expired, password changed, locked, user deleted) or that its collection no longer accepts is **cleared** and the browser is redirected to `/admin/login`; a cookie whose user lookup failed for a transient reason (database error) is kept and the request is denied, so a blip does not log everyone out. With no credential at all the request is redirected to login.

## Session lifetime

Two bounds apply to an admin session:

- **`token_expiry`** (default 2h) — the lifetime of the current cookie. Shortly before it elapses the admin UI's session dialog offers to stay signed in and calls `POST /admin/api/session-refresh`, which reissues the cookie for another `token_expiry` (sliding refresh). Refresh only succeeds for a still-valid session.
- **`[auth] session_absolute_max_age`** (default 30d, `0` to disable) — a hard ceiling measured from the original login (`auth_time` claim), regardless of how many refreshes happened. After it, refresh is refused and the user must log in again. Values above 30 days log a startup warning.

Changing the password, locking the account, or un-verifying it bumps the user's `session_version`, which invalidates every existing cookie and token immediately.

## Security

### Rate Limiting

Login and forgot-password endpoints enforce dual rate limiting — per-email and per-IP:

- **Per-email**: After `max_login_attempts` (default: 5) failed attempts, further login attempts for that email are blocked for `login_lockout_seconds` (default: 300s).
- **Per-IP**: After `max_ip_login_attempts` (default: 20) failed attempts from the same IP, all login attempts from that IP are blocked. The higher threshold tolerates shared IPs (offices, NAT).

Forgot-password requests are similarly limited per-email (`max_forgot_password_attempts`) and per-IP (`max_ip_login_attempts` with `forgot_password_window_seconds`).

The **email-verification** (`/admin/verify-email`) and **password-reset**
(`/admin/reset-password`) endpoints are rate-limited **per-IP only** (sharing the
forgot-password IP limiter), because the account isn't known until the token
resolves. Each token-consumption attempt counts toward the limit. On the reset
endpoint, the local checks (password-confirmation mismatch, password-policy
violation) run *before* the limiter, so a user's typo never consumes budget —
only genuine token attempts do. All limiters record atomically (a single
check-and-record), so concurrent attempts can't slip past the threshold.

```toml
[auth]
max_login_attempts = 5          # per-email threshold
max_ip_login_attempts = 20      # per-IP threshold (login + forgot-password)
login_lockout_seconds = "5m"    # lockout window for login
max_forgot_password_attempts = 3
forgot_password_window_seconds = "15m"
```

Rate limiting applies to the admin UI login, admin forgot-password, admin
email-verification and password-reset, and the gRPC `Login` and `ForgotPassword`
RPCs. Behind a reverse proxy, the admin UI reads the client IP from
`X-Forwarded-For`.

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

### MFA collections

(Full mode comparison, TOTP enrollment lifecycle, and security model:
[Multi-Factor Authentication](mfa.md).)

On an MFA-enabled collection, `Login` verifies the password and returns a
challenge instead of a token. For `mfa = "email"` / `"custom"` the 6-digit
code is delivered (email, or your `mfa_deliver` hook):

```json
{ "mfaRequired": true, "mfaChallenge": "eyJhbGciOi..." }
```

For `mfa = "totp"` nothing is delivered — the code comes from the user's
authenticator app. While enrollment is unconfirmed the response also
carries the provisioning URI (add it to the app; the first successful
verification confirms enrollment and the field disappears):

```json
{
  "mfaRequired": true,
  "mfaChallenge": "eyJhbGciOi...",
  "totpProvisioningUri": "otpauth://totp/crap-cms:admin%40example.com?secret=..."
}
```

Complete the login with `VerifyMfa` (the challenge token is single-purpose
and expires after 5 minutes; delivered codes are single-use, TOTP codes are
replay-guarded per time step):

```bash
grpcurl -plaintext -d '{
    "collection": "users",
    "mfa_challenge": "eyJhbGciOi...",
    "code": "123456"
}' localhost:50051 crap.ContentAPI/VerifyMfa
```

The response is the same `LoginResponse` a plain login returns (JWT +
user). Code guessing is rate-limited per identity and per IP with the same
budget as the admin MFA page.

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

The admin login form carries a `collection` field: with a single auth collection it is implied; with several, the login page shows an *account type* picker and each attempt targets exactly one collection.

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
