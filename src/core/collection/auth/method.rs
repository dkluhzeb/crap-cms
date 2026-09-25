//! Auth method variants and the `password_login` builder.

use serde::{Deserialize, Serialize};

use crate::{
    core::{
        HookRef,
        collection::{Activation, SurfaceSet},
    },
    typegen::lua::{LuaAlias, LuaTaggedClass},
};

/// MFA (Multi-Factor Authentication) mode. Lives inside the
/// `password_login` method since it only applies to the
/// email+password flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MfaMode {
    #[default]
    Off,
    /// Email-based MFA: send a 6-digit code to the user's email after password verification.
    Email,
    /// Custom delivery: the code is generated and stored by the CMS, but the
    /// `mfa_deliver` hook sends it (SMS, push, chat, …) instead of the
    /// built-in email. Verification is identical to `email`.
    Custom,
    /// Authenticator-app TOTP (RFC 6238): no code delivery at all — the user
    /// verifies against a per-user shared secret. The secret is generated on
    /// the first MFA challenge, shown as an `otpauth://` provisioning URI
    /// until the first successful verification confirms enrollment.
    Totp,
}

/// One authentication method on a collection. The collection's
/// `methods` list is ordered: the evaluator tries each in
/// declaration order, first match wins.
///
/// Serde shape: internally tagged on `type`, `snake_case`. Each
/// variant's fields appear at the same level as `type`.
///
/// ```json
/// { "type": "password_login", "mfa": "email", "verify_email": true }
/// { "type": "bearer", "surfaces": ["grpc", "admin"] }
/// { "type": "strategy", "name": "api-key", "authenticate": "hooks.auth.api_key",
///   "activates_on": { "header": "x-api-key" }, "surfaces": ["grpc"] }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, LuaTaggedClass)]
#[serde(tag = "type", rename_all = "snake_case")]
#[lua(class = "crap.AuthMethod")]
pub enum AuthMethod {
    /// Email + password login via the `Login` RPC. Issues a JWT
    /// the `Bearer` method later validates. Owns the
    /// password-only flags (`mfa`, `verify_email`,
    /// `forgot_password`) so the password-only concerns aren't
    /// scattered across the collection.
    PasswordLogin {
        /// MFA mode. `"email"` sends the code by email, `"custom"` hands it
        /// to the `mfa_deliver` hook, `"totp"` verifies against an
        /// authenticator app (no delivery); `false` (or omit) disables.
        #[serde(default)]
        #[lua(ty = "\"email\"|\"custom\"|\"totp\"|false", optional)]
        mfa: MfaMode,
        /// Optional Lua gate deciding WHETHER a verified login must complete
        /// the second factor — called after credential verification with
        /// `{ collection, user, surface, headers }`; return `false`/`nil` to
        /// skip MFA for this login, anything truthy to require it. Lets MFA
        /// apply per surface (`ctx.surface == "grpc"`) or per user field
        /// (`ctx.user.mfa_enabled`). Runs for any enabled MFA mode
        /// (`"email"`, `"custom"` or `"totp"`); no hook = MFA always required. A
        /// hook error fails CLOSED (requires MFA).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[lua(ty = "string | crap.HookRef", optional)]
        mfa_when: Option<HookRef>,
        /// Delivery hook for `mfa = "custom"`: called after credential
        /// verification with `{ collection, user, code, expires_in }` — send
        /// the code via your channel (SMS, push, …). The code is SENSITIVE:
        /// never log it. Errors are logged server-side; the previously issued
        /// code (if any) stays valid. Required with `mfa = "custom"`,
        /// rejected otherwise (startup error).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[lua(ty = "string | crap.HookRef", optional)]
        mfa_deliver: Option<HookRef>,
        /// Auth callbacks (by `{name}` of `/admin/auth/callback/[{collection}/]{name}`)
        /// whose identity provider already enforces a second factor: a
        /// session they authenticate skips this collection's MFA step. Every
        /// other callback completes the same MFA step a password login does.
        /// Only valid with an MFA mode set (startup error otherwise).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        #[lua(ty = "string[]", optional)]
        mfa_exempt_callbacks: Vec<String>,
        /// Require email verification before login (default `false`).
        #[serde(default)]
        #[lua(optional)]
        verify_email: bool,
        /// Enable the forgot-password flow (default `true`).
        #[serde(default = "default_true")]
        #[lua(optional)]
        forgot_password: bool,
    },
    /// Accept JWTs in the standard `Authorization: Bearer …`
    /// header / gRPC metadata. Default surfaces: all.
    Bearer {
        /// Surfaces this method fires on (default: `{"admin", "grpc"}`).
        #[serde(default = "SurfaceSet::all")]
        #[lua(ty = "crap.Surface[]|\"all\"", optional)]
        surfaces: SurfaceSet,
    },
    /// Accept the `crap_session` cookie. Default surfaces: admin.
    SessionCookie {
        /// Surfaces this method fires on (default: `{"admin"}`).
        #[serde(default = "SurfaceSet::admin_only")]
        #[lua(ty = "crap.Surface[]|\"all\"", optional)]
        surfaces: SurfaceSet,
    },
    /// Custom Lua-driven authentication. `authenticate` is a
    /// `module.function`-shaped Lua hook ref. The hook receives
    /// `{ headers, collection }` (matching today's contract) and
    /// returns a user document or nil.
    Strategy {
        /// Identifier used in logging + error messages. Doesn't
        /// have to be unique across collections, but should be.
        name: String,
        /// Lua function ref, e.g. `"hooks.auth.api_key"`. Receives
        /// `crap.AuthStrategyContext`; returns user doc or nil. May carry
        /// per-config options exposed to the hook as `ctx.options`.
        #[lua(ty = "string | crap.HookRef")]
        authenticate: HookRef,
        /// Discriminator for when the strategy fires. See
        /// [`Activation`].
        #[lua(ty = "crap.Activation")]
        activates_on: Activation,
        /// Surfaces this method fires on (default: `{"admin"}`).
        #[serde(default = "SurfaceSet::admin_only")]
        #[lua(ty = "crap.Surface[]|\"all\"", optional)]
        surfaces: SurfaceSet,
    },
}

fn default_true() -> bool {
    true
}

impl AuthMethod {
    /// Constructor for the common test-fixture / programmatic case
    /// of "give me a password-login method with all defaults"
    /// (`MfaMode::Off`, no verify-email, forgot-password on). Use
    /// [`Self::password_login_builder`] when you need to tweak any
    /// of those fields.
    #[must_use]
    pub fn password_login() -> Self {
        Self::PasswordLogin {
            mfa: MfaMode::Off,
            mfa_when: None,
            mfa_deliver: None,
            mfa_exempt_callbacks: Vec::new(),
            verify_email: false,
            forgot_password: true,
        }
    }

    /// Open a fluent builder for a `PasswordLogin` method. Returns
    /// a [`PasswordLoginBuilder`] — the `.mfa(...)`/`.verify_email
    /// (...)`/`.forgot_password(...)` methods are reachable only on
    /// this type, so tweak attempts on the wrong variant are a
    /// compile error instead of a silent no-op.
    ///
    /// ```ignore
    /// let method = AuthMethod::password_login_builder()
    ///     .mfa(MfaMode::Email)
    ///     .verify_email(true)
    ///     .build();
    /// ```
    #[must_use]
    pub fn password_login_builder() -> PasswordLoginBuilder {
        PasswordLoginBuilder {
            mfa: MfaMode::Off,
            mfa_when: None,
            mfa_deliver: None,
            mfa_exempt_callbacks: Vec::new(),
            verify_email: false,
            forgot_password: true,
        }
    }

    /// Constructor: bearer accepting all surfaces.
    #[must_use]
    pub fn bearer() -> Self {
        Self::Bearer {
            surfaces: SurfaceSet::all(),
        }
    }

    /// Constructor: session cookie scoped to admin (the only realistic case).
    #[must_use]
    pub fn session_cookie() -> Self {
        Self::SessionCookie {
            surfaces: SurfaceSet::admin_only(),
        }
    }
}

/// Fluent builder for a `password_login` [`AuthMethod`]. Only the
/// password-login knobs are reachable, so misusing the builder for
/// a different variant is a compile error rather than the silent
/// no-op a method-on-the-enum approach would give.
#[derive(Debug, Clone)]
pub struct PasswordLoginBuilder {
    pub(super) mfa: MfaMode,
    pub(super) mfa_when: Option<HookRef>,
    pub(super) mfa_deliver: Option<HookRef>,
    pub(super) mfa_exempt_callbacks: Vec<String>,
    pub(super) verify_email: bool,
    pub(super) forgot_password: bool,
}

impl PasswordLoginBuilder {
    /// Set the MFA mode (`Off` or `Email`). Default: `Off`.
    #[must_use]
    pub fn mfa(mut self, mode: MfaMode) -> Self {
        self.mfa = mode;
        self
    }

    /// Set the `mfa_when` gate hook. Default: `None` (MFA always required
    /// when the mode enables it).
    #[must_use]
    pub fn mfa_when(mut self, hook: Option<HookRef>) -> Self {
        self.mfa_when = hook;
        self
    }

    /// Set the `mfa_deliver` hook (required with [`MfaMode::Custom`]).
    #[must_use]
    pub fn mfa_deliver(mut self, hook: Option<HookRef>) -> Self {
        self.mfa_deliver = hook;
        self
    }

    /// Set the auth callbacks exempt from the MFA step. Default: none.
    #[must_use]
    pub fn mfa_exempt_callbacks(mut self, names: Vec<String>) -> Self {
        self.mfa_exempt_callbacks = names;
        self
    }

    /// Toggle the "require email verified before login" flag.
    /// Default: `false`.
    #[must_use]
    pub fn verify_email(mut self, value: bool) -> Self {
        self.verify_email = value;
        self
    }

    /// Toggle the "forgot-password flow available" flag. Default:
    /// `true`.
    #[must_use]
    pub fn forgot_password(mut self, value: bool) -> Self {
        self.forgot_password = value;
        self
    }

    /// Materialize the configured [`AuthMethod::PasswordLogin`].
    #[must_use]
    pub fn build(self) -> AuthMethod {
        AuthMethod::PasswordLogin {
            mfa: self.mfa,
            mfa_when: self.mfa_when,
            mfa_deliver: self.mfa_deliver,
            mfa_exempt_callbacks: self.mfa_exempt_callbacks,
            verify_email: self.verify_email,
            forgot_password: self.forgot_password,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── serde round-trips ────────────────────────────────────────────────

    #[test]
    fn json_password_login_shape() {
        let json = r#"{"type":"password_login","mfa":"email","verify_email":true}"#;
        let m: AuthMethod = serde_json::from_str(json).unwrap();
        match m {
            AuthMethod::PasswordLogin {
                mfa,
                mfa_when: _,
                mfa_deliver: _,
                mfa_exempt_callbacks,
                verify_email,
                forgot_password,
            } => {
                assert_eq!(mfa, MfaMode::Email);
                assert!(
                    mfa_exempt_callbacks.is_empty(),
                    "no callback is exempt by default"
                );
                assert!(verify_email);
                assert!(forgot_password, "forgot_password defaults to true");
            }
            other => panic!("expected PasswordLogin, got {other:?}"),
        }
    }

    #[test]
    fn json_strategy_with_header_activation() {
        let json = r#"{
            "type":"strategy",
            "name":"api-key",
            "authenticate":"hooks.auth.api_key",
            "activates_on":{"header":"x-api-key"},
            "surfaces":["grpc"]
        }"#;
        let m: AuthMethod = serde_json::from_str(json).unwrap();
        match m {
            AuthMethod::Strategy {
                name,
                authenticate,
                activates_on,
                surfaces,
            } => {
                assert_eq!(name, "api-key");
                assert_eq!(authenticate.reference(), "hooks.auth.api_key");
                assert!(
                    matches!(activates_on, Activation::Header { header } if header == "x-api-key")
                );
                assert_eq!(surfaces, SurfaceSet::grpc_only());
            }
            other => panic!("expected Strategy, got {other:?}"),
        }
    }

    #[test]
    fn json_strategy_with_always_activation() {
        let json = r#"{
            "type":"strategy",
            "name":"mtls",
            "authenticate":"hooks.auth.mtls",
            "activates_on":{"always":true}
        }"#;
        let m: AuthMethod = serde_json::from_str(json).unwrap();
        if let AuthMethod::Strategy {
            activates_on,
            surfaces,
            ..
        } = m
        {
            assert!(matches!(activates_on, Activation::Always { .. }));
            // No surfaces in JSON → defaults to admin_only.
            assert_eq!(surfaces, SurfaceSet::admin_only());
        } else {
            panic!("expected Strategy");
        }
    }

    #[test]
    fn bearer_surfaces_defaults_to_all() {
        let json = r#"{"type":"bearer"}"#;
        let m: AuthMethod = serde_json::from_str(json).unwrap();
        if let AuthMethod::Bearer { surfaces } = m {
            assert_eq!(surfaces, SurfaceSet::all());
        } else {
            panic!("expected Bearer");
        }
    }

    #[test]
    fn session_cookie_surfaces_defaults_to_admin_only() {
        let json = r#"{"type":"session_cookie"}"#;
        let m: AuthMethod = serde_json::from_str(json).unwrap();
        if let AuthMethod::SessionCookie { surfaces } = m {
            assert_eq!(surfaces, SurfaceSet::admin_only());
        } else {
            panic!("expected SessionCookie");
        }
    }
}
