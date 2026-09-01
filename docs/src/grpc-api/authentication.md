# gRPC Authentication

Whether a collection accepts a given credential is decided by its `auth.methods` list — see [Auth Methods](../authentication/auth-methods.md). The defaults (`password_login`, `bearer`, `session_cookie`) give the flow below; a collection that drops `bearer` for the `grpc` surface refuses JWTs on every RPC, including `Me`.

## Login

Authenticate with email and password to get a JWT token. Login is rate-limited — after too many failed attempts for an email (or from one IP), further attempts are temporarily blocked (configurable via `max_login_attempts`, `max_ip_login_attempts` and `login_lockout_seconds` in `crap.toml`).

```bash
grpcurl -plaintext -d '{
    "collection": "users",
    "email": "admin@example.com",
    "password": "secret123"
}' localhost:50051 crap.ContentAPI/Login
```

The response contains a `token` and the `user` document. A collection without `password_login` and without any strategy answers `PERMISSION_DENIED`; with strategies, the credentials are offered to the strategies whose `surfaces` include `grpc` and whose activation matches the request (see [Custom Strategies](../authentication/custom-strategies.md#disabling-password-login)).

## Email MFA

When the collection's `password_login` method sets `mfa = "email"` (or `"custom"`), a correct password does **not** issue a session token. Instead `Login` returns:

```json
{ "mfa_required": true, "mfa_challenge": "<short-lived challenge token>" }
```

A 6-digit code is delivered to the user (email, or your `mfa_deliver` hook). Complete the login with `VerifyMfa`:

```bash
grpcurl -plaintext -d '{
    "collection": "users",
    "mfa_challenge": "<challenge token>",
    "code": "123456"
}' localhost:50051 crap.ContentAPI/VerifyMfa
```

`VerifyMfa` returns the same `LoginResponse` shape with the real `token`. Codes are single-use and expire after 5 minutes; guesses are rate-limited per user and per IP. The challenge token carries `token_use = "mfa_pending"` and is accepted **only** by `VerifyMfa` — it cannot be used as a session, and a session token cannot be replayed into `VerifyMfa`. The optional `mfa_when` hook can skip the second factor per surface or per user.

## Bearer Token

Pass the token via the `authorization` metadata header:

```bash
grpcurl -plaintext \
    -H "authorization: Bearer eyJhbGciOi..." \
    -d '{"collection": "posts"}' \
    localhost:50051 crap.ContentAPI/Find
```

The token is validated, the user is loaded and checked (not locked, `session_version` current), and the authenticated user is available to access control functions. Custom header-activated strategies (API keys, SSO assertions) authenticate the same way — send their header instead of a JWT.

## Get Current User

The `Me` RPC resolves the caller exactly like every other RPC — from the `authorization` metadata, from any matching custom strategy, or (legacy) from the `token` field in the request body — and returns the user document with field-level access applied:

```bash
grpcurl -plaintext -H "authorization: Bearer eyJhbGciOi..." \
    -d '{}' localhost:50051 crap.ContentAPI/Me
```

`UNAUTHENTICATED` is returned for a missing, invalid, expired or revoked token, for a locked user, and for a token whose collection no longer lists `bearer` for the `grpc` surface.

## Token Expiry and Claims

Tokens expire after `token_expiry` seconds (default: 7200 = 2 hours), configurable globally in `crap.toml` or per auth collection. gRPC tokens are not refreshed — obtain a new one with `Login`. (The admin UI's cookie has a sliding refresh and an absolute ceiling, see [Login Flow](../authentication/login-flow.md#session-lifetime).)

Tokens are HS256 JWTs with these claims:

| Claim | Meaning |
|-------|---------|
| `sub` | User document id |
| `collection` | Issuing auth collection slug — a token from `users` never authenticates as `admins` |
| `email` | User email at issue time |
| `exp` | Expiry (Unix seconds) |
| `iat` | Issued-at; refreshed on every reissue |
| `auth_time` | Original login time; preserved across refreshes, drives `session_absolute_max_age` |
| `token_use` | `session` (default) or `mfa_pending` (challenge token, `VerifyMfa` only) |
| `session_version` | Must match the user's stored version; bumped on password change, lock, and un-verify, which revokes every earlier token at once |

Do not rely on the claims beyond identifying the user — they are validated server-side on every request.

## Security

- **Rate limiting** — per-email tracking. After `max_login_attempts` (default: 5) failures, the email is locked out for `login_lockout_seconds` (default: 300). Per-IP rate limiting (`max_ip_login_attempts` in `[auth]`) provides additional protection against credential stuffing across multiple accounts.
- **Timing safety** — login always performs a full Argon2id hash comparison, even for non-existent users, preventing timing-based email enumeration.
- **JWT persistence** — when no `secret` is set in `crap.toml`, an auto-generated secret is persisted to `data/.jwt_secret` so tokens survive server restarts.
- **Account locking** — when a user's `_locked` field is truthy, all authenticated requests (including `Me`) are rejected with `unauthenticated` status. This takes effect immediately, even for valid unexpired tokens, and locking also bumps `session_version` and tears down the user's live event streams.
- **Revocation** — a password change (or `LockAccount` / `UnverifyAccount`) bumps `session_version`; tokens minted before it are rejected.

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

If `password` is omitted, the existing password is kept. A successful password change bumps the user's `session_version`, so every previously issued token stops working.
