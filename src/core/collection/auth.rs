//! Authentication configuration for collections.
//!
//! ## Model
//!
//! Each auth collection declares an ordered list of `methods`, each
//! one expressing **how** a request can prove a principal for that
//! collection. Methods come in four flavors:
//!
//! - `password_login` — enables the `Login` RPC for this collection
//!   (email + password → JWT). Carries the password-only knobs:
//!   `mfa`, `verify_email`, `forgot_password`.
//! - `bearer` — accept the JWT issued by `password_login` (or a
//!   strategy) in the `Authorization: Bearer …` header / gRPC
//!   metadata. Scoped by `surfaces`.
//! - `session_cookie` — accept the `crap_session` cookie. Admin
//!   surface only in practice. Scoped by `surfaces`.
//! - `strategy` — custom Lua authenticator. Declares its own
//!   `activates_on` discriminator (header presence, or explicit
//!   `always = true`) and its `surfaces` scope.
//!
//! At request time the auth evaluator walks every collection's
//! method list in declaration order; the first method whose
//! activation matches and whose surface includes the current
//! request wins. No implicit fallback chain across collections —
//! each method is opt-in for its own activation signal.
//!
//! ## Lua shape
//!
//! ```lua
//! crap.collections.define("users", {
//!     auth = {
//!         enabled = true,
//!         methods = crap.auth.default_methods(),  -- = password_login + bearer + session_cookie
//!     },
//! })
//!
//! crap.collections.define("service_accounts", {
//!     auth = {
//!         enabled = true,
//!         methods = {
//!             { type = "strategy",
//!               name = "svc-key",
//!               authenticate = "hooks.auth.svc_key",
//!               activates_on = { header = "x-service-key" },
//!               surfaces = {"grpc"} },
//!         },
//!     },
//! })
//! ```

use serde::{Deserialize, Deserializer, Serialize};

use crate::{
    core::HookRef,
    typegen::lua::{LuaAlias, LuaAnnotation, LuaTaggedClass},
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
}

/// Which host surfaces a method can fire on. Surface filtering is
/// per-method: a method whose `surfaces` list omits the current
/// request's surface is skipped by the evaluator.
//
// Closed set — new host transports (MCP-acting-as-user, webhooks, etc.)
// would add a variant here. Per-variant Rust docs are kept internal
// (no `///`); the user-facing alias above is what the derive emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize, LuaAlias)]
#[serde(rename_all = "lowercase")]
#[lua(alias = "crap.Surface", rename_all = "lowercase")]
pub enum Surface {
    // Admin HTTP — `/admin/**` routes, middleware-driven.
    Admin,
    // gRPC `ContentAPI` — unary RPCs and `Subscribe`
    // (evaluated at stream open).
    Grpc,
}

/// Ordered list of surfaces this method is allowed on. Empty
/// means "no surface" (effectively disabled); not a default.
/// Default constructors below give sensible per-variant scopes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SurfaceSet(Vec<Surface>);

impl SurfaceSet {
    /// All known surfaces. The default for `Bearer` (the issued
    /// JWT typically works everywhere).
    #[must_use]
    pub fn all() -> Self {
        Self(vec![Surface::Admin, Surface::Grpc])
    }

    /// Admin only. Default for `SessionCookie` (cookies are an
    /// admin-HTTP concept) and `Strategy` (strategies historically
    /// fired only on admin).
    #[must_use]
    pub fn admin_only() -> Self {
        Self(vec![Surface::Admin])
    }

    /// gRPC only. Useful for machine-to-machine auth collections.
    #[must_use]
    pub fn grpc_only() -> Self {
        Self(vec![Surface::Grpc])
    }

    /// Construct from an explicit list of surfaces. Used by parsers
    /// converting Lua-table strings to typed surfaces; prefer
    /// [`Self::all`] / [`Self::admin_only`] / [`Self::grpc_only`]
    /// for programmatic construction.
    #[must_use]
    pub fn from_list(surfaces: Vec<Surface>) -> Self {
        Self(surfaces)
    }

    /// True iff `surface` is in this set.
    #[must_use]
    pub fn contains(&self, surface: Surface) -> bool {
        self.0.contains(&surface)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Iterate the contained surfaces in declaration order.
    pub fn iter(&self) -> std::slice::Iter<'_, Surface> {
        self.0.iter()
    }
}

impl<'a> IntoIterator for &'a SurfaceSet {
    type Item = &'a Surface;
    type IntoIter = std::slice::Iter<'a, Surface>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

/// How a `Strategy` method is triggered for a given request.
///
/// `Always` is the explicit catch-all escape hatch (`mTLS`, multi-
/// signal strategies, `IdP` introspection). Required by the
/// validator to be the literal `{ always = true }` table —
/// `{ always = false }` is rejected at deserialize-time via the
/// custom `deserialize_true_only` helper. Use a `Header`
/// discriminator when the strategy fires on a specific request
/// header, which is the common case for API-key / SSO patterns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, LuaTaggedClass)]
#[serde(untagged)]
#[lua(class = "crap.Activation")]
pub enum Activation {
    /// Strategy is invoked on every request that passes the surface
    /// filter. The strategy itself decides per-request whether to
    /// authenticate. Emits a startup warning to make accidental
    /// always-active strategies loud.
    Always {
        /// Must be `true`. `false` is rejected at deserialize time.
        #[serde(deserialize_with = "deserialize_true_only")]
        #[lua(ty = "true")]
        always: bool,
    },
    /// Strategy fires only when the named header (lowercase, gRPC
    /// metadata or HTTP header) is present on the request.
    Header {
        /// Header name (lowercase) — e.g. `"x-api-key"`.
        header: String,
    },
}

impl Activation {
    /// Construct an `Always`-active activation. Use sparingly;
    /// prefer header-discriminated activation so each strategy is
    /// bound to its own request signal.
    #[must_use]
    pub fn always() -> Self {
        Activation::Always { always: true }
    }

    /// Construct a header-discriminated activation.
    #[must_use]
    pub fn header(name: impl Into<String>) -> Self {
        Activation::Header {
            header: name.into(),
        }
    }
}

fn deserialize_true_only<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    let value = bool::deserialize(d)?;
    if !value {
        return Err(serde::de::Error::custom(
            "activates_on.always must be `true`; set to `false` is meaningless. \
             Remove the method or use an explicit `header` discriminator instead.",
        ));
    }
    Ok(true)
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
        /// Email-MFA mode. `"email"` enables; `false` (or omit) disables.
        #[serde(default)]
        #[lua(ty = "\"email\"|false", optional)]
        mfa: MfaMode,
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
        #[lua(ty = "crap.Surface[]", optional)]
        surfaces: SurfaceSet,
    },
    /// Accept the `crap_session` cookie. Default surfaces: admin.
    SessionCookie {
        /// Surfaces this method fires on (default: `{"admin"}`).
        #[serde(default = "SurfaceSet::admin_only")]
        #[lua(ty = "crap.Surface[]", optional)]
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
        #[lua(ty = "crap.Surface[]", optional)]
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

    /// Constructor: `Always`-active strategy on admin surface
    /// (matches the legacy `auth.strategies = […]` behavior for
    /// pre-`methods` configs).
    #[must_use]
    pub fn strategy_always(name: impl Into<String>, authenticate: impl Into<HookRef>) -> Self {
        Self::Strategy {
            name: name.into(),
            authenticate: authenticate.into(),
            activates_on: Activation::always(),
            surfaces: SurfaceSet::admin_only(),
        }
    }

    /// Constructor: header-discriminated strategy.
    #[must_use]
    pub fn strategy_on_header(
        name: impl Into<String>,
        authenticate: impl Into<HookRef>,
        header: impl Into<String>,
        surfaces: SurfaceSet,
    ) -> Self {
        Self::Strategy {
            name: name.into(),
            authenticate: authenticate.into(),
            activates_on: Activation::Header {
                header: header.into(),
            },
            surfaces,
        }
    }
}

/// Fluent builder for a `password_login` [`AuthMethod`]. Only the
/// password-login knobs are reachable, so misusing the builder for
/// a different variant is a compile error rather than the silent
/// no-op a method-on-the-enum approach would give.
#[derive(Debug, Clone, Copy)]
pub struct PasswordLoginBuilder {
    mfa: MfaMode,
    verify_email: bool,
    forgot_password: bool,
}

impl PasswordLoginBuilder {
    /// Set the MFA mode (`Off` or `Email`). Default: `Off`.
    #[must_use]
    pub fn mfa(mut self, mode: MfaMode) -> Self {
        self.mfa = mode;
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
            verify_email: self.verify_email,
            forgot_password: self.forgot_password,
        }
    }
}

/// Top-level authentication configuration for a collection.
///
/// `enabled = true` + non-empty `methods` makes this an auth
/// collection — provisions `_password_hash` and friends in the
/// schema, registers it as a target for `Login` / `Me` / etc.
///
/// `methods` is required (no implicit defaults). Use
/// `crap.auth.default_methods()` from Lua for the common
/// password+bearer+cookie set.
#[derive(Debug, Clone, Serialize, Deserialize, LuaAnnotation)]
#[lua(class = "crap.Auth")]
pub struct Auth {
    /// Enable auth for this collection. Required true when `methods` is non-empty.
    #[lua(optional)]
    pub enabled: bool,
    /// JWT lifetime in seconds (default: 7200).
    #[serde(default = "default_token_expiry")]
    #[lua(optional)]
    pub token_expiry: u64,
    /// Ordered list of auth methods. Use `crap.auth.default_methods()` for the standard set or `crap.auth.with_defaults({...})` to extend it.
    #[serde(default)]
    #[lua(optional, ty = "crap.AuthMethod[]")]
    pub methods: Vec<AuthMethod>,
}

fn default_token_expiry() -> u64 {
    7200
}

impl Auth {
    /// Create a new auth config with the given enabled flag.
    /// Methods must be set separately.
    #[must_use]
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            ..Default::default()
        }
    }

    /// The standard default method set:
    ///   1. `password_login` (no MFA, no `verify_email`, `forgot_password = true`)
    ///   2. `bearer` (all surfaces)
    ///   3. `session_cookie` (admin only)
    ///
    /// Mirrors `crap.auth.default_methods()` on the Lua side.
    #[must_use]
    pub fn default_methods() -> Vec<AuthMethod> {
        vec![
            AuthMethod::PasswordLogin {
                mfa: MfaMode::Off,
                verify_email: false,
                forgot_password: true,
            },
            AuthMethod::Bearer {
                surfaces: SurfaceSet::all(),
            },
            AuthMethod::SessionCookie {
                surfaces: SurfaceSet::admin_only(),
            },
        ]
    }

    /// Find the `password_login` method's configuration, if
    /// present. `None` means this collection does not accept
    /// email/password login at all (the `Login` RPC returns
    /// `INVALID_ARGUMENT` for this collection).
    #[must_use]
    pub fn password_login(&self) -> Option<PasswordLoginCfg> {
        self.methods.iter().find_map(|m| match m {
            AuthMethod::PasswordLogin {
                mfa,
                verify_email,
                forgot_password,
            } => Some(PasswordLoginCfg {
                mfa: *mfa,
                verify_email: *verify_email,
                forgot_password: *forgot_password,
            }),
            _ => None,
        })
    }

    /// True iff the `bearer` method is present and covers `surface`.
    #[must_use]
    pub fn accepts_bearer(&self, surface: Surface) -> bool {
        self.methods.iter().any(|m| match m {
            AuthMethod::Bearer { surfaces } => surfaces.contains(surface),
            _ => false,
        })
    }

    /// True iff the `session_cookie` method is present and covers `surface`.
    #[must_use]
    pub fn accepts_session_cookie(&self, surface: Surface) -> bool {
        self.methods.iter().any(|m| match m {
            AuthMethod::SessionCookie { surfaces } => surfaces.contains(surface),
            _ => false,
        })
    }

    /// True iff this collection's `password_login` method has
    /// `verify_email = true` (or there's no `password_login` at all
    /// when called in contexts where "must verify" is the default —
    /// this returns false for missing `password_login`).
    #[must_use]
    pub fn requires_verify_email(&self) -> bool {
        self.password_login().is_some_and(|c| c.verify_email)
    }

    /// True iff this collection's `password_login` method has
    /// `forgot_password = true`. Missing `password_login` → false
    /// (the flow needs Login to even start).
    #[must_use]
    pub fn forgot_password_enabled(&self) -> bool {
        self.password_login().is_some_and(|c| c.forgot_password)
    }

    /// True iff this collection has a `password_login` method —
    /// i.e. accepts email + password via the `Login` flow. Inverse
    /// of the old `disable_local` flag.
    #[must_use]
    pub fn password_login_enabled(&self) -> bool {
        self.password_login().is_some()
    }

    /// Get the configured MFA mode (or `Off` when no `password_login`).
    #[must_use]
    pub fn mfa(&self) -> MfaMode {
        self.password_login().map_or(MfaMode::Off, |c| c.mfa)
    }

    /// True iff this collection has at least one `strategy` method.
    #[must_use]
    pub fn has_strategies(&self) -> bool {
        self.strategies().next().is_some()
    }

    // ── Fluent builders for tests / programmatic construction ────────

    /// `Auth { enabled = true, methods = default_methods() }` —
    /// the standard "password + bearer + cookie" shape.
    #[must_use]
    pub fn enabled() -> Self {
        Self {
            enabled: true,
            token_expiry: default_token_expiry(),
            methods: Self::default_methods(),
        }
    }

    /// Re-build the `password_login` method via the given builder
    /// closure. The closure receives a [`PasswordLoginBuilder`]
    /// seeded with the current method's settings, and the returned
    /// builder replaces the existing entry.
    ///
    /// No-op when no `password_login` method is present — prefer
    /// chaining on [`AuthMethod::password_login_builder`] when you
    /// want compile-time guarantees the method exists.
    #[must_use]
    pub fn map_password_login(
        mut self,
        f: impl FnOnce(PasswordLoginBuilder) -> PasswordLoginBuilder,
    ) -> Self {
        if let Some(idx) = self
            .methods
            .iter()
            .position(|m| matches!(m, AuthMethod::PasswordLogin { .. }))
            && let AuthMethod::PasswordLogin {
                mfa,
                verify_email,
                forgot_password,
            } = self.methods[idx]
        {
            let seed = PasswordLoginBuilder {
                mfa,
                verify_email,
                forgot_password,
            };
            self.methods[idx] = f(seed).build();
        }
        self
    }

    /// Append a strategy method built via [`AuthMethod::strategy_always`].
    #[must_use]
    pub fn with_strategy(
        mut self,
        name: impl Into<String>,
        authenticate: impl Into<HookRef>,
    ) -> Self {
        self.methods
            .push(AuthMethod::strategy_always(name, authenticate));
        self
    }

    /// Iterate all `strategy` methods on this collection in their
    /// declared `methods`-list order.
    ///
    /// Order within a collection is deterministic. Order *across*
    /// collections is `HashMap` iteration order (see
    /// [`service::auth::evaluate`](crate::service::auth::evaluate)
    /// for the cross-collection caveat) — callers that walk
    /// strategies of all collections in succession get a stable
    /// ordering only per-collection.
    pub fn strategies(&self) -> impl Iterator<Item = StrategyCfg<'_>> {
        self.methods.iter().filter_map(|m| match m {
            AuthMethod::Strategy {
                name,
                authenticate,
                activates_on,
                surfaces,
            } => Some(StrategyCfg {
                name,
                authenticate,
                activates_on,
                surfaces,
            }),
            _ => None,
        })
    }
}

impl Default for Auth {
    fn default() -> Self {
        Self {
            enabled: false,
            token_expiry: default_token_expiry(),
            methods: Vec::new(),
        }
    }
}

/// Borrowed view of a `password_login` method's config.
#[derive(Debug, Clone, Copy)]
pub struct PasswordLoginCfg {
    pub mfa: MfaMode,
    pub verify_email: bool,
    pub forgot_password: bool,
}

/// Borrowed view of a `strategy` method's config.
#[derive(Debug, Clone, Copy)]
pub struct StrategyCfg<'a> {
    pub name: &'a str,
    pub authenticate: &'a HookRef,
    pub activates_on: &'a Activation,
    pub surfaces: &'a SurfaceSet,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── shape + defaults ─────────────────────────────────────────────────

    #[test]
    fn collection_auth_defaults_disabled_with_empty_methods() {
        let auth = Auth::default();
        assert!(!auth.enabled);
        assert_eq!(auth.token_expiry, 7200);
        assert!(auth.methods.is_empty());
    }

    #[test]
    fn default_methods_has_password_bearer_cookie_in_order() {
        let methods = Auth::default_methods();
        assert_eq!(methods.len(), 3);
        assert!(matches!(methods[0], AuthMethod::PasswordLogin { .. }));
        assert!(matches!(methods[1], AuthMethod::Bearer { .. }));
        assert!(matches!(methods[2], AuthMethod::SessionCookie { .. }));
    }

    #[test]
    fn default_methods_bearer_covers_all_surfaces() {
        let auth = Auth {
            enabled: true,
            methods: Auth::default_methods(),
            ..Default::default()
        };
        assert!(auth.accepts_bearer(Surface::Admin));
        assert!(auth.accepts_bearer(Surface::Grpc));
    }

    #[test]
    fn default_methods_cookie_covers_admin_only() {
        let auth = Auth {
            enabled: true,
            methods: Auth::default_methods(),
            ..Default::default()
        };
        assert!(auth.accepts_session_cookie(Surface::Admin));
        assert!(!auth.accepts_session_cookie(Surface::Grpc));
    }

    #[test]
    fn password_login_accessor_extracts_config() {
        let auth = Auth {
            enabled: true,
            methods: vec![AuthMethod::PasswordLogin {
                mfa: MfaMode::Email,
                verify_email: true,
                forgot_password: false,
            }],
            ..Default::default()
        };
        let cfg = auth.password_login().expect("present");
        assert_eq!(cfg.mfa, MfaMode::Email);
        assert!(cfg.verify_email);
        assert!(!cfg.forgot_password);
    }

    #[test]
    fn password_login_absent_when_method_not_listed() {
        let auth = Auth {
            enabled: true,
            methods: vec![AuthMethod::Bearer {
                surfaces: SurfaceSet::all(),
            }],
            ..Default::default()
        };
        assert!(auth.password_login().is_none());
    }

    #[test]
    fn strategies_iter_yields_only_strategy_variants() {
        let auth = Auth {
            enabled: true,
            methods: vec![
                AuthMethod::Bearer {
                    surfaces: SurfaceSet::all(),
                },
                AuthMethod::Strategy {
                    name: "api-key".into(),
                    authenticate: "hooks.auth.api_key".into(),
                    activates_on: Activation::Header {
                        header: "x-api-key".into(),
                    },
                    surfaces: SurfaceSet::grpc_only(),
                },
                AuthMethod::Strategy {
                    name: "sso".into(),
                    authenticate: "hooks.auth.sso".into(),
                    activates_on: Activation::always(),
                    surfaces: SurfaceSet::admin_only(),
                },
            ],
            ..Default::default()
        };
        let names: Vec<&str> = auth.strategies().map(|s| s.name).collect();
        assert_eq!(names, vec!["api-key", "sso"]);
    }

    // ── serde round-trips ────────────────────────────────────────────────

    #[test]
    fn json_round_trip_default_methods() {
        let auth = Auth {
            enabled: true,
            methods: Auth::default_methods(),
            ..Default::default()
        };
        let json = serde_json::to_string(&auth).unwrap();
        let back: Auth = serde_json::from_str(&json).unwrap();
        assert_eq!(back.methods.len(), 3);
    }

    #[test]
    fn json_password_login_shape() {
        let json = r#"{"type":"password_login","mfa":"email","verify_email":true}"#;
        let m: AuthMethod = serde_json::from_str(json).unwrap();
        match m {
            AuthMethod::PasswordLogin {
                mfa,
                verify_email,
                forgot_password,
            } => {
                assert_eq!(mfa, MfaMode::Email);
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
    fn activation_always_false_is_rejected() {
        // The sentinel rule prevents `{ always = false }` from being a
        // valid disabled-strategy declaration — it should be expressed
        // by removing the method entirely or using a header discriminator.
        let json = r#"{"always":false}"#;
        let err = serde_json::from_str::<Activation>(json).unwrap_err();
        assert!(
            err.to_string().contains("did not match any variant")
                || err.to_string().contains("must be `true`"),
            "expected helpful error, got: {err}"
        );
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
