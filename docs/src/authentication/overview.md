# Authentication

Crap CMS provides built-in authentication via auth-enabled collections. Any collection can serve as an auth collection by setting `auth = true`.

## Key Concepts

- **Auth collection** — a collection with `auth = true`. Users are regular documents in this collection.
- **Two auth surfaces** — Admin UI uses an HttpOnly cookie (`crap_session`). gRPC API uses Bearer tokens.
- **JWT** — all tokens are JWT signed with the configured secret (or an auto-generated one persisted to `data/.jwt_secret`).
- **Argon2id** — passwords are hashed with Argon2id before storage.
- **Rate limiting** — login endpoints enforce per-email rate limiting (configurable max attempts and lockout window).
- **Timing-safe** — login always performs a password hash comparison, even when the user doesn't exist, to prevent timing-based email enumeration.
- **CSRF protection** — admin UI forms and HTMX requests are protected with double-submit cookie tokens.
- **Secure cookies** — the `crap_session` cookie includes the `Secure` flag in production (when `dev_mode = false`).
- **`_password_hash`** — a hidden column added to auth collection tables. Never exposed in API responses, hooks, or admin forms.
- **Custom strategies** — pluggable auth via Lua functions (API keys, LDAP, SSO).
- **Password reset** — token-based forgot/reset password flow via admin UI and gRPC. Requires email configuration.
- **Email verification** — optional per-collection. When enabled, users must verify their email before logging in.

## Activation

Auth middleware activates when at least one auth collection exists **or** when `admin.require_auth` is `true` (the default). This means:

- **`require_auth = true` (default):** If no auth collections are defined, the admin shows a "Setup Required" page. Create an auth collection and bootstrap a user to proceed.
- **`require_auth = false`:** If no auth collections are defined, the admin is fully open (dev/prototyping mode).
- **Auth collection exists:** Standard authentication applies. Optionally, `admin.access` can further restrict which authenticated users can access the admin panel.

## Quick Setup

1. Define an auth collection:

```lua
-- collections/users.lua
crap.collections.define("users", {
    auth = true,
    fields = {
        crap.fields.text({ name = "name", required = true }),
        crap.fields.select({ name = "role", options = {
            { label = "Admin", value = "admin" },
            { label = "Editor", value = "editor" },
        }}),
    },
})
```

2. Bootstrap the first user:

```bash
crap-cms -C ./my-project user create -e admin@example.com
```

3. (Optional) Set a JWT secret explicitly, or let it auto-generate and persist to `data/.jwt_secret`:

```toml
[auth]
secret = "your-random-secret-here"  # omit to auto-generate (persisted across restarts)
```

4. (Optional) Configure email for password reset and verification:

```toml
[email]
smtp_host = "smtp.example.com"
smtp_port = 587
smtp_user = "noreply@example.com"
smtp_pass = "your-smtp-password"
from_address = "noreply@example.com"
from_name = "My App"
```

5. (Optional) Enable email verification:

```lua
crap.collections.define("users", {
    auth = {
        verify_email = true,
    },
    fields = { ... },
})
```

## Password Reset

When email is configured, a "Forgot password?" link appears on the admin login page. The flow:

1. User clicks "Forgot password?" and enters their email
2. Server generates a single-use reset token (nanoid, stored in DB with 1-hour expiry). Login rate limiting applies — too many failed attempts will temporarily lock out the email.
3. Reset email is sent with a link to `/admin/reset-password?token=xxx`
4. User sets a new password via the form
5. Token is consumed (single-use) and user is redirected to login

Available via gRPC as `ForgotPassword` and `ResetPassword` RPCs.

**Security:** The forgot password endpoint always returns success regardless of whether the email exists, to prevent user enumeration.

## Email Verification

When `verify_email: true` is set on an auth collection:

1. A verification email is automatically sent when a user is created (admin UI or gRPC)
2. The email contains a link to `/admin/verify-email?token=xxx`
3. Verification tokens expire after 24 hours
4. Until verified, the user cannot log in (returns "Please verify your email" error)
5. Clicking the verification link marks the user as verified (if token hasn't expired)

Available via gRPC as `VerifyEmail` RPC.

**Note:** Email verification requires SMTP to be configured. If SMTP is not configured, verification emails won't be sent (logged as warnings) and unverified users will be unable to log in.
