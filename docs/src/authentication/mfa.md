# Multi-Factor Authentication

Auth collections can require a second factor after password verification.
Three modes exist, set on the `password_login` method:

| Mode | Second factor | Delivery | Best for |
|---|---|---|---|
| `mfa = "email"` | 6-digit single-use code | Built-in email | Simple setups with reliable email |
| `mfa = "custom"` | 6-digit single-use code | Your `mfa_deliver` hook (SMS, push, chat, …) | Existing messaging infrastructure |
| `mfa = "totp"` | 6-digit authenticator-app code (RFC 6238) | None — codes are computed on the user's device | No delivery dependency at all |

```lua
auth = {
    enabled = true,
    methods = {
        { type = "password_login", mfa = "totp" },
        { type = "bearer" },
        { type = "session_cookie" },
    },
}
```

An unknown `mfa` value is a **load error** — a typo can never silently
disable the second factor.

## Shared machinery (all modes)

Every mode rides the same challenge flow on both login surfaces (the admin
login page and the gRPC `Login` RPC) and on the admin
[auth callbacks](#auth-callbacks-oauth--oidc):

1. The password (or the custom strategy, or the callback's identity
   provider) is verified first. No token or session is issued.
2. The optional `mfa_when` hook decides whether THIS login needs the
   second factor (per surface, per user field, …). No hook = always
   required; a hook error fails closed.
3. A short-lived (5 minute) **MFA-pending token** binds the verified
   login to the completion step. It is purpose-bound: it cannot be used
   as a session token, and a session token cannot be replayed into the
   MFA step. It is also bound to the surface that issued it: a challenge
   from the gRPC `Login` completes only through `VerifyMfa`, one from the
   admin login or a callback only on `/admin/mfa`.
4. The code is verified on `/admin/mfa` or via the `VerifyMfa` RPC —
   both call the same verification chokepoint, and both share the same
   per-identity and per-IP guess limiters.
5. The session minted after the second factor records that it passed it
   (see [Sessions carry the second factor](#sessions-carry-the-second-factor)).

## Email and custom delivery

`Login` generates a 6-digit code, stores it (single-use, 5-minute
expiry — any verification attempt consumes it, right or wrong), and
delivers it: built-in email for `"email"`, your hook for `"custom"`.
Consumption is atomic: of several attempts submitted at the same moment,
exactly one is judged against the code and the rest are refused, so a
burst of guesses still gets one guess per issued code:

```lua
{ type = "password_login", mfa = "custom", mfa_deliver = "hooks.mfa.send_sms" },

-- hooks/mfa.lua — ctx: { collection, user, code, expires_in }
function M.send_sms(ctx)
    sms_provider.send(ctx.user.phone, "Your code: " .. ctx.code)
end
```

`mfa = "custom"` without `mfa_deliver` (or the hook without the mode) is
a startup error. The code is stored before the hook runs, and the hook holds
no database connection while it delivers: its `crap.*` CRUD (to record the
send, or enqueue it as a job) takes a write connection at its first call, in
one transaction that commits when the hook returns and rolls back when it
raises. Code **issuance** is throttled per user so a
password-holder cannot flood the delivery channel by looping the login
form: `max_forgot_password_attempts` codes within
`forgot_password_window_seconds`, on the admin login and gRPC `Login` alike.
Over budget, the login is refused until the window passes.

## TOTP (authenticator apps)

`mfa = "totp"` verifies against a per-user shared secret instead of a
delivered code. Standard parameters — SHA-1, 30-second steps, 6 digits,
±1 step of clock tolerance — so every common authenticator app works.

### Enrollment lifecycle

Enrollment is **challenge-driven**: there is no separate setup page.

1. **First MFA challenge** (first login after enabling the mode): the
   server generates a 160-bit secret, stores it sealed, and shows the
   provisioning material — on the admin MFA page as an `otpauth://`
   link plus the base32 manual key, and on gRPC as the
   `totp_provisioning_uri` field of the `LoginResponse`.
2. The user adds it to their authenticator app and submits the current
   code. The **first successful verification confirms enrollment**.
3. From then on the provisioning material is never shown again — the
   MFA step is just "enter your authenticator code". An unconfirmed
   (half-finished) enrollment re-shows the *same* secret on the next
   challenge, so setup is resumable.

```json
// gRPC Login on an unenrolled TOTP account:
{
  "mfaRequired": true,
  "mfaChallenge": "eyJhbGciOi...",
  "totpProvisioningUri": "otpauth://totp/crap-cms:admin%40example.com?secret=JBSW...&issuer=crap-cms&digits=6&period=30"
}
```

### Security model

- **Sealed storage.** The secret is stored AES-256-GCM-encrypted, keyed
  from `[auth] secret` with a TOTP-specific context — a database leak
  alone does not expose TOTP secrets, and the ciphertext is not
  interchangeable with `crap.crypto.encrypt` payloads.
- **Replay guard.** The last accepted time step is persisted; a code
  never verifies twice, and codes from already-used or older steps are
  rejected. Verification shares the MFA guess limiters with the other
  modes.
- **Enrollment is trust-on-first-login.** The provisioning link is
  issued to whoever completes the *password* step while enrollment is
  unconfirmed. If a password leaks **before** the legitimate user
  enrolls, the attacker can enroll their own authenticator — or,
  stealthier, *record* the provisioning secret without confirming and
  let the legitimate user enroll the same secret later: confirmation
  does **not** invalidate provisioning material that was already
  observed while unconfirmed. Treat enrollment as part of account
  handover: have users complete their first login (and thereby
  enrollment) promptly, prefer creating accounts with a forced
  password reset, and reset enrollment (below) if a pre-enrollment
  password leak is suspected. Both provisioning and confirmation emit
  server audit logs (`TOTP enrollment provisioned` / `confirmed`), so
  an enrollment the user did not perform is detectable in the logs.
- **Secret rotation.** Rotating `[auth] secret` makes stored TOTP
  secrets unopenable: verification fails closed, and the next challenge
  **restarts enrollment** (fresh secret, provisioning shown again).
  Plan rotations accordingly — every TOTP user re-enrolls.

### Resetting a user's enrollment

An operator resets a user's enrollment with the CLI (lost authenticator,
suspected pre-enrollment password leak, …):

```bash
crap-cms user reset-totp -e admin@example.com          # prompts to confirm
crap-cms user reset-totp -c editors --id abc123 -y     # non-interactive
```

The next login challenge re-provisions from scratch. Like a password
change, the reset ends the user's existing sessions — every cookie and
token issued before it is rejected — and, with Redis live updates, tears
down their open live streams on `serve`. Resetting re-opens
the trust-on-first-login window, so pair it with a password change when
compromise is suspected. (The underlying `_totp_*` columns are system
columns — never exposed through the API, hooks, or admin forms.)

### Current limitations

- No backup/recovery codes — a lost authenticator means an operator
  reset (above).
- Per-user opt-in is expressed through `mfa_when` (e.g.
  `return ctx.user.mfa_enabled == true`), not a built-in flag.

## `mfa_when` — scoping the second factor

Any mode can be gated per login:

```lua
{ type = "password_login", mfa = "totp", mfa_when = "hooks.auth.mfa_when" },

-- hooks/auth.lua: MFA for admin logins only
function M.mfa_when(ctx)
    return ctx.surface == "admin"
end
```

`ctx` carries `{ collection, user, surface, headers }`. A hook error
fails closed (MFA required). The hook is a predicate, so its `crap.*` CRUD
is **read-only**: reads (`find`, `find_by_id`, `count`, …) work, and any
write (`create`, `update`, `delete`, `crap.transaction`, `crap.jobs.queue`,
…) raises an error naming the gate — which, like any hook error, fails
closed.

### Sessions carry the second factor

Every session token records the surface that minted it and whether it
passed the second factor (an MFA-exempt callback counts as passed — its
identity provider enforced one). A session that did **not** pass it is
refused on every request whose gate requires the second factor: the
collection's `mfa` mode and `mfa_when` are judged again **for that
request** — its surface and headers. With the example above, a user who
logs in over gRPC gets a token without the second factor; that token keeps
working on gRPC, but the admin refuses it, as a bearer token and as the
session cookie alike, and the user must sign in on the admin (completing
the MFA step) to use it. A session that passed the second factor works on
every surface the collection accepts it on.

So `mfa_when` runs not only at login but also on each request a
no-second-factor session authenticates on an MFA collection — keep it
cheap, and base it on stable facts (surface, user fields) rather than
anything that would make a signed-in user's requests flap between
accepted and refused. A request refused this way answers like any invalid
credential: the admin clears the cookie and redirects to the login; gRPC
answers `UNAUTHENTICATED` (`Second factor required on this surface`).

## Auth callbacks (OAuth / OIDC)

A session minted by an [auth callback](custom-strategies.md#auth-callbacks-oauth2--oidc)
(`/admin/auth/callback/[{collection}/]{name}`) passes the **same** MFA
step: on an MFA collection the callback redirects to `/admin/mfa` with a
pending-MFA cookie — the user completes the collection's mode (TOTP, email
code, or custom delivery) exactly as after a password login — and only then
gets the session. `mfa_when` runs for callbacks too (`surface = "admin"`,
the callback request's headers).

When the identity provider already enforces a second factor, list the
callback by name so its logins skip this collection's MFA step:

```lua
{ type = "password_login", mfa = "totp", mfa_exempt_callbacks = { "okta" } },
```

The name is the `{name}` of the callback route (the file
`auth_callback/{name}.lua`). Only listed callbacks skip the step; every
other callback, and every password or strategy login, still completes it.
`mfa_exempt_callbacks` without an `mfa` mode is a startup error. Exempt a
callback only when the provider *enforces* 2FA for every account that can
reach it — the exemption trusts the provider's second factor in place of
this collection's.
